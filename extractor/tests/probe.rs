//! Golden-spine tier 1: extraction correctness on the pinned toolchain.
//!
//! Drives the freshly built extractor dylib over `tests/probes/expansion` via
//! the embedded `dylint::run` (the exact mechanism the production orchestrator
//! uses) and asserts the macro-expansion span policy on the emitted fragment:
//! hand-written `pub` tokens get byte-exact `vis_span`s; macro- and
//! derive-generated items carry no editable `vis_span` and map their whole-item
//! `span` to the invocation site (never the macro definition file `gen.rs`).
//!
//! Ported from the spike's `wl-probe-check` binary (WS1, SPIKE §12.7/§12b);
//! the assertion set is the 21-check suite that verified the policy, plus the
//! schema-version gate. One `#[test]` only: the flow chdirs into the probe
//! (dylint checks the CWD workspace), which must not race a sibling test.

use std::path::{Path, PathBuf};
use std::process::Command;

use dylint::opts::{Check, Dylint, LibrarySelection, Operation};
use wl_ir::{IrFragment, ItemFact, Visibility};

#[test]
fn expansion_probe_span_policy() -> anyhow::Result<()> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let probe_root = manifest_dir.join("tests/probes/expansion");

    // Build the dylib explicitly: with `crate-type = ["cdylib"]` the test binary
    // doesn't link the lib target, so `cargo test` alone doesn't guarantee a
    // fresh artifact. The inner build respects rust-toolchain.toml (cwd) and
    // dylint_linting's build script emits the `@<toolchain>` suffixed name.
    let status = Command::new("cargo")
        .arg("build")
        .current_dir(&manifest_dir)
        .status()?;
    anyhow::ensure!(status.success(), "cargo build of the extractor failed");
    let lib_path = find_dylib(&manifest_dir.join("target/debug"))?;

    // Force a re-lint (the SPIKE §11 caching gotcha): WL_IR_OUT is not in
    // cargo's fingerprint, so with a warm dylint cache the lint pass is skipped
    // as "fresh" and nothing lands in the new temp dir. Bumping the dylib mtime
    // invalidates exactly the probe's lint units — the same mechanism as the
    // orchestrator's completeness guard (`force_relint` in the spike embed).
    // Scoped: the handle must be CLOSED before `dylint::run` — Windows refuses
    // to load a dylib any process holds open for write (sharing violation),
    // and this write handle would otherwise live to the end of the test.
    {
        let lib_file = std::fs::OpenOptions::new().append(true).open(&lib_path)?;
        lib_file.set_modified(std::time::SystemTime::now())?;
    }

    // The embed flow, verbatim from the spike orchestrator: WL_IR_OUT is
    // inherited by the spawned driver; dylint checks the CWD workspace.
    let ir_out = tempfile::tempdir()?;
    // SAFETY: single-threaded at this point — this file holds exactly one test,
    // and cargo runs each integration-test binary as its own process.
    unsafe { std::env::set_var("WL_IR_OUT", ir_out.path()) };
    std::env::set_current_dir(&probe_root)?;

    let opts = Dylint {
        pipe_stderr: None,
        pipe_stdout: None,
        quiet: false,
        operation: Operation::Check(Check {
            lib_sel: LibrarySelection {
                lib_paths: vec![lib_path.to_string_lossy().into_owned()],
                ..Default::default()
            },
            no_deps: true,
            ..Default::default()
        }),
    };
    dylint::run(&opts)?;

    let frag_path = ir_out.path().join("probe_expansion.json");
    let frag: IrFragment = serde_json::from_str(&std::fs::read_to_string(&frag_path)?)?;
    frag.check_schema().map_err(anyhow::Error::msg)?;

    let mut ck = Checker {
        root: probe_root,
        failures: Vec::new(),
        passes: 0,
    };

    // 1. Hand-written plain `pub fn plain` — byte-exact vis_span, text "pub",
    //    not an expansion.
    ck.check("plain", &frag, |c, it| {
        c.expect(
            !span_from_expansion(it),
            it,
            "span must not be from expansion",
        );
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
        c.expect(
            it.vis_span.is_none(),
            it,
            "vis_span must be None (token in macro def)",
        );
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
                    && matches!(
                        it.path.last().map(String::as_str),
                        Some("clone") | Some("fmt")
                    )
            })
            .collect();
        if derived.is_empty() {
            ck.failures.push(
                "[derive assoc items] none found (expected Clone::clone / Debug::fmt)".into(),
            );
        }
        for it in derived {
            ck.expect(
                span_from_expansion(it),
                it,
                "derive span must be from expansion",
            );
            ck.expect_span_file_ends(it, "lib.rs");
            ck.expect(
                it.vis_span.is_none(),
                it,
                "derive assoc vis_span must be None",
            );
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

    // 7. (PR 9) Export attrs: `#[no_mangle]` lands in `ItemFact::attrs` — the
    //    reachability root evidence for FFI exports.
    ck.check("ffi_export", &frag, |c, it| {
        c.expect(
            it.attrs.iter().any(|a| a == "no_mangle"),
            it,
            "attrs must carry no_mangle",
        );
    });
    ck.check("plain", &frag, |c, it| {
        c.expect(it.attrs.is_empty(), it, "no export attrs on a plain fn");
    });

    // 8. (PR 9) Span lines: 1-based, computed by the extractor. Expected
    //    values come from the fixture source itself (self-maintaining), and
    //    the cross-file macro's line must be the INVOCATION line.
    let src = std::fs::read_to_string(ck.root.join("src/lib.rs"))?;
    let line_of = |needle: &str| -> u32 {
        (src.lines().position(|l| l.contains(needle)).unwrap() + 1) as u32
    };
    ck.check("plain", &frag, |c, it| {
        let expected = line_of("pub fn plain()");
        let got = it.span.as_ref().map_or(0, |s| s.line);
        c.expect(
            got == expected,
            it,
            &format!("span.line == {expected} (got {got})"),
        );
    });
    ck.check("from_cross_file_macro", &frag, |c, it| {
        let expected = line_of("make_pub_fn!(from_cross_file_macro)");
        let got = it.span.as_ref().map_or(0, |s| s.line);
        c.expect(
            got == expected,
            it,
            &format!("span.line == invocation line {expected} (got {got})"),
        );
    });

    // 9. (PR 9) Signature exposure: `Probed` is named in `exposes_probed`'s
    //    signature — the lowered-signature pass must emit an in_signature edge.
    {
        let hit = frag.references.iter().any(|e| {
            e.in_signature
                && e.from.last().map(String::as_str) == Some("exposes_probed")
                && e.to.last().map(String::as_str) == Some("Probed")
        });
        if hit {
            ck.passes += 1;
            println!("PASS  exposes_probed: in_signature edge to Probed");
        } else {
            ck.failures
                .push("[exposes_probed] missing in_signature edge to Probed".into());
        }
    }

    // 10. CRLF fidelity (the `.gitattributes`-pinned `src/crlf.rs`): span
    //    offsets must be ON-DISK bytes. rustc normalizes CRLF→LF while
    //    loading, and a span emitted in normalized coordinates slices one
    //    byte early per preceding `\r` — exactly what a Windows
    //    `core.autocrlf=true` checkout produced before the
    //    `original_relative_byte_pos` mapping. Byte-exact vis text is the
    //    `--fix` write-surface guarantee.
    ck.check_nested("crlf_probed", &frag, |c, it| {
        c.expect(
            !span_from_expansion(it),
            it,
            "span must not be from expansion",
        );
        c.expect_vis_text(it, "pub");
    });
    ck.check_nested("crlf_crate_only", &frag, |c, it| {
        c.expect_vis_text(it, "pub(crate)");
    });

    // 11. (PR 10) `self_type`: an inherent-impl item links its nominal self
    //     type by stable key — including from an impl block in a DIFFERENT
    //     module (`def_path_str` renders that method at the impl's module, so
    //     only the key link can recover the type). Trait-impl items carry
    //     `trait_item` instead, never `self_type`.
    {
        let by_name = |name: &str| -> Option<&ItemFact> {
            frag.items
                .iter()
                .find(|it| it.path.last().map(String::as_str) == Some(name))
        };
        let carrier_key = by_name("Carrier").map(|it| it.key.clone());
        match carrier_key {
            None => ck.failures.push("[Carrier] item not found".into()),
            Some(carrier_key) => {
                for method in ["same_module", "remote_method"] {
                    match by_name(method) {
                        None => ck.failures.push(format!("[{method}] item not found")),
                        Some(it) => {
                            let it = it.clone();
                            ck.expect(
                                it.self_type.as_deref() == Some(carrier_key.as_str()),
                                &it,
                                "self_type must be Carrier's key",
                            );
                            ck.expect(it.trait_item.is_none(), &it, "inherent: no trait_item");
                        }
                    }
                }
            }
        }
        // A derive-generated trait-impl assoc fn must NOT carry self_type.
        let derived = frag.items.iter().find(|it| {
            it.trait_item.is_some() && it.path.last().map(String::as_str) == Some("clone")
        });
        match derived {
            None => ck
                .failures
                .push("[self_type] no trait-impl item to counter-check".into()),
            Some(it) => {
                let it = it.clone();
                ck.expect(
                    it.self_type.is_none(),
                    &it,
                    "trait-impl item must not carry self_type",
                );
            }
        }
    }

    // 12. Global invariant (load-bearing): nothing in the fragment references
    //    gen.rs except the `make_pub_fn` macro definition itself (which genuinely
    //    lives there, is a `macro`, and is not from an expansion).
    for it in &frag.items {
        let touches_gen = it
            .span
            .as_ref()
            .is_some_and(|s| ends_with(&s.file, "gen.rs"))
            || it
                .vis_span
                .as_ref()
                .is_some_and(|s| ends_with(&s.file, "gen.rs"));
        if touches_gen {
            let is_macro_def =
                it.kind == "macro" && it.path.last().map(String::as_str) == Some("make_pub_fn");
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
    anyhow::ensure!(
        ck.failures.is_empty(),
        "span-policy assertions failed:\n{}",
        ck.failures.join("\n")
    );
    Ok(())
}

/// Locate the toolchain-suffixed dylib dylint_linting's build script produced.
/// Prefix and extension are per-OS (`libwl_extractor@….dylib`/`.so`,
/// `wl_extractor@….dll`); the extension filter keeps Windows' sibling
/// `.dll.lib`/`.pdb` artifacts out.
fn find_dylib(dir: &Path) -> anyhow::Result<PathBuf> {
    for entry in std::fs::read_dir(dir)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let prefixed = name.starts_with("libwl_extractor@") || name.starts_with("wl_extractor@");
        let dylib_ext = name.ends_with(".dylib") || name.ends_with(".so") || name.ends_with(".dll");
        if prefixed && dylib_ext {
            return Ok(entry.path());
        }
    }
    anyhow::bail!("no wl_extractor@<toolchain> dylib under {}", dir.display());
}

struct Checker {
    root: PathBuf,
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

    /// [`Checker::check`] for an item one module deep (`[crate, mod, name]`).
    fn check_nested(&mut self, name: &str, frag: &IrFragment, f: impl Fn(&mut Checker, &ItemFact)) {
        let matches: Vec<&ItemFact> = frag
            .items
            .iter()
            .filter(|it| it.path.last().map(String::as_str) == Some(name) && it.path.len() == 3)
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
        let got = it
            .span
            .as_ref()
            .map(|s| s.file.clone())
            .unwrap_or_else(|| "<none>".into());
        self.expect(ok, it, &format!("span file ends with {suffix} (got {got})"));
    }

    fn expect_span_file_not(&mut self, it: &ItemFact, suffix: &str) {
        let bad = it.span.as_ref().is_some_and(|s| ends_with(&s.file, suffix));
        self.expect(!bad, it, &format!("span file must NOT be {suffix}"));
    }

    /// Assert the vis_span exists and the source bytes under it equal `expected`.
    fn expect_vis_text(&mut self, it: &ItemFact, expected: &str) {
        let Some(vs) = &it.vis_span else {
            self.expect(
                false,
                it,
                &format!("vis_span present with text {expected:?}"),
            );
            return;
        };
        let path = self.root.join(&vs.file);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                self.expect(false, it, &format!("read {}: {e}", path.display()));
                return;
            }
        };
        let (lo, hi) = (vs.lo as usize, vs.hi as usize);
        let got = bytes
            .get(lo..hi)
            .map(|b| String::from_utf8_lossy(b).into_owned());
        let ok = got.as_deref() == Some(expected);
        self.expect(
            ok,
            it,
            &format!(
                "vis_span text == {expected:?} (got {:?})",
                got.unwrap_or_default()
            ),
        );
    }
}

fn span_from_expansion(it: &ItemFact) -> bool {
    it.span.as_ref().is_some_and(|s| s.from_expansion)
}

fn ends_with(file: &str, suffix: &str) -> bool {
    file.replace('\\', "/").ends_with(suffix)
}
