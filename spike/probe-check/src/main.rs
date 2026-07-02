//! WS1 span-fidelity probe checker (SPIKE §12.7).
//!
//! Usage: `wl-probe-check <IR_JSON> <PROBE_ROOT>`
//!
//! Loads the IR fragment the extractor produced for `spike/probes/expansion`
//! and asserts the macro-expansion span policy holds: hand-written `pub` tokens
//! get byte-exact `vis_span`s; macro- and derive-generated items carry no
//! editable `vis_span` and map their whole-item `span` to the invocation site
//! (never the macro definition file `gen.rs`). Exits non-zero, listing every
//! failed assertion, so it doubles as a regression gate.

use std::path::Path;
use std::process::ExitCode;

use wl_ir::{IrFragment, ItemFact, Visibility};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let (Some(ir_json), Some(probe_root)) = (args.next(), args.next()) else {
        eprintln!("usage: wl-probe-check <IR_JSON> <PROBE_ROOT>");
        return ExitCode::FAILURE;
    };

    let frag: IrFragment = match std::fs::read_to_string(&ir_json)
        .map_err(|e| e.to_string())
        .and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
        .and_then(|f: IrFragment| f.check_schema().map(|()| f))
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("wl-probe-check: cannot read {ir_json}: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut ck = Checker {
        root: probe_root,
        failures: Vec::new(),
        passes: 0,
    };

    // 1. Hand-written plain `pub fn plain` — byte-exact vis_span, text "pub",
    //    not an expansion.
    ck.check("plain", &frag, |c, it| {
        c.expect(!span_from_expansion(it), it, "span must not be from expansion");
        c.expect_vis_text(it, "pub");
    });

    // 2. Cross-file macro output: item exists as a public fn (a fidelity win —
    //    syn can't see it), whole-item span maps to the *invocation* file
    //    (lib.rs), NOT the macro definition (gen.rs), and it has no vis_span.
    ck.check("from_cross_file_macro", &frag, |c, it| {
        c.expect(it.kind == "fn", it, "kind must be fn");
        c.expect(it.visibility == Visibility::Public, it, "must be Public");
        c.expect_span_file_ends(it, "lib.rs");
        c.expect_span_file_not(it, "gen.rs");
        c.expect(it.vis_span.is_none(), it, "vis_span must be None (token in macro def)");
    });

    // 3. Same-file macro output: same policy (still an expansion).
    ck.check("from_local_macro", &frag, |c, it| {
        c.expect(it.kind == "fn", it, "kind must be fn");
        c.expect(it.visibility == Visibility::Public, it, "must be Public");
        c.expect_span_file_ends(it, "lib.rs");
        c.expect(it.vis_span.is_none(), it, "vis_span must be None");
    });

    // 4. Derive-generated trait-impl assoc items (`Clone::clone`, `Debug::fmt`):
    //    expansion-derived → span from_expansion, no vis_span. Found positionally
    //    (trait-impl assoc items on the derives), not by exact path rendering.
    {
        let derived: Vec<&ItemFact> = frag
            .items
            .iter()
            .filter(|it| {
                it.parent_kind.as_deref() == Some("impl")
                    && it.trait_item.is_some()
                    && matches!(it.path.last().map(String::as_str), Some("clone") | Some("fmt"))
            })
            .collect();
        if derived.is_empty() {
            ck.failures
                .push("[derive assoc items] none found (expected Clone::clone / Debug::fmt)".into());
        }
        for it in derived {
            ck.check_item(&format!("derive::{}", it.path.join("::")), it, |c, it| {
                c.expect(span_from_expansion(it), it, "derive span must be from expansion");
                c.expect_span_file_ends(it, "lib.rs");
                c.expect(it.vis_span.is_none(), it, "derive assoc vis_span must be None");
            });
        }
    }

    // 5. Restricted `pub(crate) fn crate_only` — rustc captures the token syn
    //    can't; text must be exactly "pub(crate)".
    ck.check("crate_only", &frag, |c, it| {
        c.expect(
            it.visibility == Visibility::Restricted("crate".into()),
            it,
            "must be Restricted(crate)",
        );
        c.expect_vis_text(it, "pub(crate)");
    });

    // 6. Private `fn private` — inherited visibility lowers to an empty span → None.
    ck.check("private", &frag, |c, it| {
        c.expect(it.vis_span.is_none(), it, "private vis_span must be None");
    });

    // 7. Global invariant (load-bearing): nothing in the fragment references
    //    gen.rs except the `make_pub_fn` macro definition itself (which genuinely
    //    lives there, is a `macro`, and is not from an expansion).
    for it in &frag.items {
        let touches_gen = it.span.as_ref().is_some_and(|s| ends_with(&s.file, "gen.rs"))
            || it.vis_span.as_ref().is_some_and(|s| ends_with(&s.file, "gen.rs"));
        if touches_gen {
            let is_macro_def = it.kind == "macro" && it.path.last().map(String::as_str) == Some("make_pub_fn");
            if !is_macro_def {
                ck.failures.push(format!(
                    "[global invariant] {} references gen.rs but is not the macro def",
                    it.path.join("::")
                ));
            } else {
                ck.passes += 1;
                println!("PASS  global-invariant: only `make_pub_fn` macro def lives in gen.rs");
            }
        }
    }

    println!("\n{} passed, {} failed", ck.passes, ck.failures.len());
    if ck.failures.is_empty() {
        ExitCode::SUCCESS
    } else {
        for f in &ck.failures {
            eprintln!("FAIL  {f}");
        }
        ExitCode::FAILURE
    }
}

struct Checker {
    root: String,
    failures: Vec<String>,
    passes: usize,
}

impl Checker {
    /// Find the unique top-level item whose last path segment is `name`, run the
    /// assertions against it, or record a not-found failure.
    fn check(&mut self, name: &str, frag: &IrFragment, f: impl Fn(&mut Checker, &ItemFact)) {
        let matches: Vec<&ItemFact> = frag
            .items
            .iter()
            .filter(|it| it.path.last().map(String::as_str) == Some(name) && it.path.len() == 2)
            .collect();
        match matches.as_slice() {
            [it] => {
                let it = (*it).clone();
                f(self, &it);
            }
            [] => self.failures.push(format!("[{name}] item not found")),
            many => self
                .failures
                .push(format!("[{name}] expected 1 item, found {}", many.len())),
        }
    }

    fn check_item(&mut self, label: &str, it: &ItemFact, f: impl Fn(&mut Checker, &ItemFact)) {
        let _ = label;
        f(self, it);
    }

    fn expect(&mut self, cond: bool, it: &ItemFact, msg: &str) {
        let label = it.path.join("::");
        if cond {
            self.passes += 1;
            println!("PASS  {label}: {msg}");
        } else {
            self.failures.push(format!("{label}: {msg}"));
        }
    }

    fn expect_span_file_ends(&mut self, it: &ItemFact, suffix: &str) {
        let ok = it.span.as_ref().is_some_and(|s| ends_with(&s.file, suffix));
        let got = it.span.as_ref().map(|s| s.file.clone()).unwrap_or_else(|| "<none>".into());
        self.expect(ok, it, &format!("span file ends with {suffix} (got {got})"));
    }

    fn expect_span_file_not(&mut self, it: &ItemFact, suffix: &str) {
        let bad = it.span.as_ref().is_some_and(|s| ends_with(&s.file, suffix));
        self.expect(!bad, it, &format!("span file must NOT be {suffix}"));
    }

    /// Assert the vis_span exists and the source bytes under it equal `expected`.
    fn expect_vis_text(&mut self, it: &ItemFact, expected: &str) {
        let Some(vs) = &it.vis_span else {
            self.expect(false, it, &format!("vis_span present with text {expected:?}"));
            return;
        };
        let path = Path::new(&self.root).join(&vs.file);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                self.expect(false, it, &format!("read {}: {e}", path.display()));
                return;
            }
        };
        let (lo, hi) = (vs.lo as usize, vs.hi as usize);
        let got = bytes.get(lo..hi).map(|b| String::from_utf8_lossy(b).into_owned());
        let ok = got.as_deref() == Some(expected);
        self.expect(
            ok,
            it,
            &format!("vis_span text == {expected:?} (got {:?})", got.unwrap_or_default()),
        );
    }
}

fn span_from_expansion(it: &ItemFact) -> bool {
    it.span.as_ref().is_some_and(|s| s.from_expansion)
}

fn ends_with(file: &str, suffix: &str) -> bool {
    file.replace('\\', "/").ends_with(suffix)
}
