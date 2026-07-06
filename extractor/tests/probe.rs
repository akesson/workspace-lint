//! Golden-spine tier 1: extraction correctness on the pinned toolchain.
//!
//! Drives the freshly built extractor dylib over `tests/probes/expansion` via
//! the embedded `dylint::run` (the exact mechanism the production orchestrator
//! uses) and asserts the macro-expansion span policy on the emitted fragment:
//! hand-written `pub` tokens get byte-exact `vis_span`s; macro- and
//! derive-generated items carry no editable `vis_span` and map their whole-item
//! `span` to the invocation site (never the macro definition file `gen.rs`).
//!
//! Ported from the retired pivot spike's `wl-probe-check` binary (WS1);
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
    // orchestrator's completeness guard (`force_relint`).
    // Scoped: the handle must be CLOSED before `dylint::run` — Windows refuses
    // to load a dylib any process holds open for write (sharing violation),
    // and this write handle would otherwise live to the end of the test.
    {
        let lib_file = std::fs::OpenOptions::new().append(true).open(&lib_path)?;
        lib_file.set_modified(std::time::SystemTime::now())?;
    }

    // The embed flow, same as the production orchestrator: WL_IR_OUT is
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
            // The invocation-site policy: the span lands in whichever probe
            // file WROTE the `#[derive(…)]` (lib.rs, inherent.rs), never a
            // macro-definition file.
            ck.expect(
                it.span
                    .as_ref()
                    .is_some_and(|s| s.file.ends_with("lib.rs") || s.file.ends_with("inherent.rs")),
                it,
                "derive span must map to the deriving probe file",
            );
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

    // 12. (clippy-unmask guard) `self_kind` / `self_copy`: an assoc fn's
    //     receiver classification and its self type's `Copy`-ness — the
    //     substrate the stable side replays clippy's `wrong_self_convention`
    //     table against before `--fix` narrows an item out of
    //     `avoid-breaking-exported-api`. Non-assoc defs carry neither.
    {
        let by_name = |name: &str| -> Option<&ItemFact> {
            frag.items
                .iter()
                .find(|it| it.path.last().map(String::as_str) == Some(name))
        };
        let expect_self =
            |ck: &mut Checker, name: &str, kind: &str, copy: Option<bool>| match by_name(name) {
                None => ck.failures.push(format!("[{name}] item not found")),
                Some(it) => {
                    let it = it.clone();
                    ck.expect(
                        it.self_kind.as_deref() == Some(kind) && it.self_copy == copy,
                        &it,
                        "self_kind/self_copy must match the written receiver",
                    );
                }
            };
        expect_self(&mut ck, "same_module", "ref", Some(false));
        expect_self(&mut ck, "is_heavy", "value", Some(false));
        expect_self(&mut ck, "to_units", "value", Some(true));
        match by_name("Carrier") {
            None => ck.failures.push("[Carrier] item not found".into()),
            Some(it) => {
                let it = it.clone();
                ck.expect(
                    it.self_kind.is_none() && it.self_copy.is_none(),
                    &it,
                    "non-assoc defs carry no self_kind/self_copy",
                );
            }
        }
        // Field facts + field-READ edges (the dead-field narrow guard's
        // substrate): `Carrier.value` is a field def, and `self.value` in
        // `same_module` emits a read edge to it.
        let field = frag.items.iter().find(|it| {
            it.kind == "field" && it.path.ends_with(&["Carrier".into(), "value".into()])
        });
        match field {
            None => ck
                .failures
                .push("[Carrier::value] field fact not emitted".into()),
            Some(it) => {
                let key = it.key.clone();
                let read = frag
                    .references
                    .iter()
                    .any(|e| e.to_key == key && !e.import && e.receiver_resolved);
                if read {
                    ck.passes += 1;
                    println!("PASS  Carrier::value: field fact + receiver-resolved read edge");
                } else {
                    ck.failures.push(
                        "[Carrier::value] no receiver-resolved read edge to the field".into(),
                    );
                }
            }
        }
        // `receiver_resolved` use-site discrimination (`call_shapes`): the
        // method CALL `c.same_module()` is receiver-based (`true`); the
        // written `Chip::to_units(chip)` path resolves the type by name
        // (`false`). The dangling-import check keys on exactly this split.
        let from_calls = |name: &str| {
            frag.references.iter().find(|e| {
                e.from.last().map(String::as_str) == Some("call_shapes")
                    && e.to.last().map(String::as_str) == Some(name)
                    && !e.import
            })
        };
        let mut expect_receiver = |name: &str, want: bool, msg: &str| match from_calls(name) {
            None => ck
                .failures
                .push(format!("[call_shapes] missing edge to {name}")),
            Some(e) if e.receiver_resolved == want => {
                ck.passes += 1;
                println!("PASS  call_shapes→{name}: {msg}");
            }
            Some(_) => ck.failures.push(format!("call_shapes→{name}: {msg}")),
        };
        expect_receiver(
            "same_module",
            true,
            "a method call is receiver-resolved (no written path)",
        );
        expect_receiver(
            "to_units",
            false,
            "a written `Chip::to_units` path is NOT receiver-resolved",
        );

        // `from_module` (schema 6): the edge from the trait-impl body
        // (`<Chip as Yardstick>::raw` → `helper`) must carry the impl's
        // LEXICAL module — which `from` itself hides inside the bracket
        // rendering, the property that forced the dedicated field.
        let raw_edge = frag.references.iter().find(|e| {
            e.from.last().map(String::as_str) == Some("raw")
                && e.to.last().map(String::as_str) == Some("helper")
                && !e.import
        });
        match raw_edge {
            None => ck
                .failures
                .push("[lexical] missing edge raw → helper".into()),
            Some(e) => {
                let bracketed = e.from.iter().any(|s| s.starts_with('<'));
                let module_ok = e.from_module.join("::").ends_with("inherent::lexical");
                if bracketed && module_ok {
                    ck.passes += 1;
                    println!(
                        "PASS  lexical::raw→helper: bracket-rendered `from` carries lexical from_module"
                    );
                } else {
                    ck.failures.push(format!(
                        "[lexical] raw→helper: from={:?} (bracketed: {bracketed}) from_module={:?} (ends with inherent::lexical: {module_ok})",
                        e.from, e.from_module
                    ));
                }
            }
        }
    }

    // 13. (PR 11) Fragment target kind: the probe compiles as a plain lib —
    //     the cargo-env discriminator (`CARGO_BIN_NAME` / `CARGO_TARGET_TMPDIR`
    //     both absent) must land on "lib".
    if frag.target_kind == "lib" {
        ck.passes += 1;
        println!("PASS  fragment: target_kind == \"lib\"");
    } else {
        ck.failures.push(format!(
            "[fragment] target_kind must be \"lib\", got {:?}",
            frag.target_kind
        ));
    }

    // 14. (PR 11) Use-site spans + glob discrimination on reference edges.
    {
        let edge =
            |pred: &dyn Fn(&wl_ir::RefEdge) -> bool| frag.references.iter().find(|e| pred(e));
        let last = |v: &[String]| v.last().cloned().unwrap_or_default();

        // A glob import (`use super::globbed::*`) → module edge with glob=true,
        // anchored at the `use` line.
        let glob_line = line_of("use super::globbed::*");
        match edge(&|e| e.import && last(&e.from) == "glob_user" && last(&e.to) == "globbed") {
            None => ck
                .failures
                .push("[glob_user] missing import edge to `globbed`".into()),
            Some(e) => {
                let got = e.span.as_ref().map_or(0, |s| s.line);
                if e.glob && got == glob_line {
                    ck.passes += 1;
                    println!("PASS  glob_user: glob import edge, span.line == use line");
                } else {
                    ck.failures.push(format!(
                        "[glob_user] want glob=true span.line={glob_line}, \
                         got glob={} span.line={got}",
                        e.glob
                    ));
                }
            }
        }

        // A named module import (`use super::globbed`) resolves to the SAME
        // module def — glob must stay false (the discrimination this exists for).
        match edge(&|e| e.import && last(&e.from) == "named_user" && last(&e.to) == "globbed") {
            None => ck
                .failures
                .push("[named_user] missing import edge to `globbed`".into()),
            Some(e) => {
                if e.glob {
                    ck.failures
                        .push("[named_user] plain module import must have glob=false".into());
                } else {
                    ck.passes += 1;
                    println!("PASS  named_user: named module import, glob == false");
                }
            }
        }

        // A renamed single import (`use … as renamed_target`) records the local
        // binding — the only place the IR carries a `use a::B as C` alias.
        match edge(&|e| e.import && last(&e.from) == "renamed_user" && last(&e.to) == "glob_target")
        {
            None => ck
                .failures
                .push("[renamed_user] missing import edge to `glob_target`".into()),
            Some(e) => {
                if e.alias.as_deref() == Some("renamed_target") {
                    ck.passes += 1;
                    println!("PASS  renamed_user: import alias == renamed_target");
                } else {
                    ck.failures.push(format!(
                        "[renamed_user] want alias=Some(\"renamed_target\"), got {:?}",
                        e.alias
                    ));
                }
            }
        }

        // A body call site carries the use-site span (the architecture anchor).
        let call_line = line_of("probe: use-site-anchor");
        match edge(&|e| {
            !e.import && last(&e.from) == "calls_glob_target" && last(&e.to) == "glob_target"
        }) {
            None => ck
                .failures
                .push("[calls_glob_target] missing call edge to `glob_target`".into()),
            Some(e) => {
                let got = e.span.as_ref().map_or(0, |s| s.line);
                if got == call_line {
                    ck.passes += 1;
                    println!("PASS  calls_glob_target: call edge span.line == call line");
                } else {
                    ck.failures.push(format!(
                        "[calls_glob_target] want span.line={call_line}, got {got}"
                    ));
                }
            }
        }

        // A macro-invocation edge's span projects to the INVOCATION line and
        // keeps the from_expansion marker (architecture classifies these).
        let invoke_line = line_of("make_pub_fn!(from_cross_file_macro)");
        match edge(&|e| last(&e.to) == "make_pub_fn") {
            None => ck
                .failures
                .push("[macro edge] missing edge to `make_pub_fn`".into()),
            Some(e) => {
                let (got, from_exp) = e
                    .span
                    .as_ref()
                    .map_or((0, false), |s| (s.line, s.from_expansion));
                if got == invoke_line && from_exp {
                    ck.passes += 1;
                    println!("PASS  make_pub_fn: macro edge at invocation line, from_expansion");
                } else {
                    ck.failures.push(format!(
                        "[macro edge] want span.line={invoke_line} from_expansion=true, \
                         got line={got} from_expansion={from_exp}"
                    ));
                }
            }
        }
    }

    // 15. Global invariant (load-bearing): nothing in the fragment references
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

    // 16. (gap fix) Written-root recording on re-export shims (`RefEdge::via`).
    //     `shim` re-exports `std::time::Duration`: the resolved target lives in
    //     core/std, so the edge's `to[0]` can never credit the dep — only the
    //     recorded written root can (the `web-time` unused-deps FP class).
    {
        let edge =
            |pred: &dyn Fn(&wl_ir::RefEdge) -> bool| frag.references.iter().find(|e| pred(e));
        let last = |v: &[String]| v.last().cloned().unwrap_or_default();

        // The `use shim::Duration` import edge: resolved into the std family,
        // written root recorded.
        match edge(&|e| e.import && last(&e.from) == "via_user" && last(&e.to) == "Duration") {
            None => ck
                .failures
                .push("[via_user] missing import edge to `Duration`".into()),
            Some(e) => {
                let std_family = matches!(e.to.first().map(String::as_str), Some("std" | "core"));
                if e.via.as_deref() == Some("shim") && std_family {
                    ck.passes += 1;
                    println!("PASS  via_user: shim import records via=shim, to[0] in std family");
                } else {
                    ck.failures.push(format!(
                        "[via_user] want via=Some(\"shim\") to[0] in {{std,core}}, \
                         got via={:?} to={:?}",
                        e.via, e.to
                    ));
                }
            }
        }

        // The `use shim::OwnItem` import edge: written root IS the defining
        // crate — via must stay None.
        match edge(&|e| e.import && last(&e.from) == "via_user" && last(&e.to) == "OwnItem") {
            None => ck
                .failures
                .push("[via_user] missing import edge to `OwnItem`".into()),
            Some(e) => {
                if e.via.is_none() {
                    ck.passes += 1;
                    println!("PASS  via_user: direct shim import records via=None");
                } else {
                    ck.failures.push(format!(
                        "[via_user] direct use must have via=None, got {:?}",
                        e.via
                    ));
                }
            }
        }

        // The fully-qualified code path `shim::Duration::from_secs(0)`: the
        // non-`use` shape must record the written root too.
        match edge(&|e| !e.import && last(&e.from) == "wait" && e.via.as_deref() == Some("shim")) {
            None => ck.failures.push(
                "[via_user::wait] missing non-import edge with via=shim \
                 (fully-qualified path shape)"
                    .into(),
            ),
            Some(_) => {
                ck.passes += 1;
                println!("PASS  via_user: fully-qualified code path records via=shim");
            }
        }
    }

    // 17. (gap fix) Build-script extraction: the probe's build.rs compiles as
    //     crate `build_script_build` through the same wrapper; the extractor
    //     must emit a references-only fragment keyed on the owning package.
    //     (Key-join against the stub is deliberately NOT asserted here — the
    //     stub is a `no_deps` dependency with no fragment, and build.rs edges
    //     carry Build-mode keys; the assembler's path-fallback join is
    //     covered by wl-engine's golden tests and the cases fixture.)
    {
        let build_path = ir_out.path().join("probe_expansion@build.json");
        match std::fs::read_to_string(&build_path) {
            Err(e) => ck
                .failures
                .push(format!("[build fragment] missing {build_path:?}: {e}")),
            Ok(text) => {
                let bfrag: IrFragment = serde_json::from_str(&text)?;
                bfrag.check_schema().map_err(anyhow::Error::msg)?;
                if bfrag.target_kind == "build"
                    && bfrag.crate_name == "build_script_build"
                    && bfrag.items.is_empty()
                {
                    ck.passes += 1;
                    println!(
                        "PASS  build fragment: references-only, target_kind=build, \
                         crate_name=build_script_build"
                    );
                } else {
                    ck.failures.push(format!(
                        "[build fragment] want target_kind=build crate_name=build_script_build \
                         items=[], got kind={:?} crate={:?} items={}",
                        bfrag.target_kind,
                        bfrag.crate_name,
                        bfrag.items.len()
                    ));
                }
                // The build.rs call into the stub is a real (non-import) edge.
                match bfrag
                    .references
                    .iter()
                    .find(|e| !e.import && e.to.last().map(String::as_str) == Some("build_helper"))
                {
                    Some(e) if e.to.first().map(String::as_str) == Some("buildstub") => {
                        ck.passes += 1;
                        println!("PASS  build fragment: edge to buildstub::build_helper");
                    }
                    Some(e) => ck.failures.push(format!(
                        "[build fragment] build_helper edge with unexpected to: {:?}",
                        e.to
                    )),
                    None => ck
                        .failures
                        .push("[build fragment] missing edge to buildstub::build_helper".into()),
                }
            }
        }
    }

    // 18. (import surgery) `decl_span` / `elem_span` — the unused-pub `--fix`
    //     deletion surfaces, and the brace discriminator the lint relies on:
    //       * standalone `use a::b;` → `decl_span` is the WHOLE statement and
    //         strictly contains `elem_span` (`decl != elem`) → delete the
    //         statement.
    //       * brace-list leaf → rustc collapses the leaf item's span to the
    //         leaf, so `decl_span == elem_span` → excise the leaf in place.
    //     `elem_span` must cover the whole brace entry as written, including a
    //     nested path (`deep::buried`) or an `as`-rename — never just the last
    //     segment (that would leave `deep::` behind).
    {
        let root = ck.root.clone();
        let slice = |sp: &wl_ir::Span| -> String {
            let bytes = std::fs::read(root.join(&sp.file)).unwrap_or_default();
            bytes
                .get(sp.lo as usize..sp.hi as usize)
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .unwrap_or_default()
        };
        let same = |a: &wl_ir::Span, b: &wl_ir::Span| a.lo == b.lo && a.hi == b.hi;
        let last = |v: &[String]| v.last().cloned().unwrap_or_default();
        // (from, to, braced, want_elem, want_decl_if_standalone)
        let cases = [
            // Standalone: decl ⊋ elem, decl is the whole `use …;`.
            (
                "named_user",
                "globbed",
                false,
                "super::globbed",
                "use super::globbed;",
            ),
            (
                "renamed_user",
                "glob_target",
                false,
                "super::globbed::glob_target as renamed_target",
                "use super::globbed::glob_target as renamed_target;",
            ),
            // Brace-list leaves: decl == elem (leaf-collapsed).
            ("list_user", "first", true, "first", ""),
            ("list_user", "second", true, "second", ""),
            ("list_user", "aliased_src", true, "aliased_src as al", ""),
            // Nested path inside a brace: elem is the whole entry `deep::buried`.
            ("nested_user", "buried", true, "deep::buried", ""),
            ("nested_user", "shallow", true, "shallow", ""),
        ];
        let mut results: Vec<Result<String, String>> = Vec::new();
        for (from, to, braced, want_elem, want_decl) in cases {
            let found = frag
                .references
                .iter()
                .find(|e| e.import && last(&e.from) == from && last(&e.to) == to);
            match found {
                None => results.push(Err(format!("[{from} -> {to}] missing import edge"))),
                Some(e) => {
                    let (Some(decl), Some(elem)) = (&e.decl_span, &e.elem_span) else {
                        results.push(Err(format!(
                            "[{from} -> {to}] want both spans, got decl={:?} elem={:?}",
                            e.decl_span, e.elem_span
                        )));
                        continue;
                    };
                    let elem_text = slice(elem);
                    let is_braced = same(decl, elem);
                    let elem_ok = elem_text == want_elem;
                    let braced_ok = is_braced == braced;
                    // For a standalone import the whole-statement delete surface
                    // must be exactly `decl_span`.
                    let decl_ok = braced || slice(decl) == want_decl;
                    if elem_ok && braced_ok && decl_ok {
                        results.push(Ok(format!(
                            "{from}->{to}: braced={braced} elem={want_elem:?}"
                        )));
                    } else {
                        results.push(Err(format!(
                            "[{from} -> {to}] want braced={braced} elem={want_elem:?} \
                             decl={want_decl:?}, got braced={is_braced} elem={elem_text:?} \
                             decl={:?}",
                            slice(decl)
                        )));
                    }
                }
            }
        }
        for r in results {
            match r {
                Ok(m) => {
                    ck.passes += 1;
                    println!("PASS  {m}");
                }
                Err(e) => ck.failures.push(e),
            }
        }
    }

    // 19. (deletion surface) `full_span` is the WHOLE item — leading doc
    //     comments and attributes through the body's closing brace — not
    //     `def_span` (the signature). Deleting `def_span` would orphan the body
    //     `{ … }` and leave a dangling `///`; `full_span` removes the item
    //     cleanly. Sliced byte-exact against `plain` (documented, with a body).
    {
        let src = std::fs::read_to_string(ck.root.join("src/lib.rs"))?;
        let plain = frag
            .items
            .iter()
            .find(|it| it.path.last().map(String::as_str) == Some("plain") && it.path.len() == 2);
        match plain.and_then(|it| it.full_span.as_ref()) {
            None => ck
                .failures
                .push("[plain] full_span missing (deletion surface)".into()),
            Some(fs) => {
                let text = src.get(fs.lo as usize..fs.hi as usize).unwrap_or("");
                let ok = text.starts_with("///")
                    && text.contains("pub fn plain() -> u32")
                    && text.trim_end().ends_with('}');
                if ok {
                    ck.passes += 1;
                    println!("PASS  plain: full_span spans doc-comment..body-close");
                } else {
                    ck.failures
                        .push(format!("[plain] full_span text unexpected: {text:?}"));
                }
            }
        }
        // Attributed fn: the delete surface must cover the doc comment and the
        // outer attributes so a delete leaves nothing orphaned.
        let attributed = frag.items.iter().find(|it| {
            it.path.last().map(String::as_str) == Some("attributed") && it.path.len() == 2
        });
        match attributed.and_then(|it| it.full_span.as_ref()) {
            None => ck.failures.push("[attributed] full_span missing".into()),
            Some(fs) => {
                let text = src.get(fs.lo as usize..fs.hi as usize).unwrap_or("");
                let ok = text.trim_start().starts_with("///")
                    && text.contains("#[inline]")
                    && text.contains("#[allow")
                    && text.trim_end().ends_with('}');
                if ok {
                    ck.passes += 1;
                    println!("PASS  attributed: full_span covers doc + attrs + body");
                } else {
                    ck.failures
                        .push(format!("[attributed] full_span text: {text:?}"));
                }
            }
        }
    }

    // 20. Proc-macro entry points carry the `proc_macro` export attr — the
    //     assembler's `Reach::ExportRoot` substrate (LeaveDates validation
    //     2026-07-05: without the root, the synthesized `_DECLS` registration
    //     edge reads as intra-crate usage and `--fix` narrows the entry fn, a
    //     hard compile error). Second dylint run, own lint-target probe crate
    //     (`proc-macro = true` can't share ../expansion's lib).
    {
        std::env::set_current_dir(manifest_dir.join("tests/probes/procmacro"))?;
        dylint::run(&opts)?;
        let frag_path = ir_out.path().join("probe_procmacro.json");
        let pm: IrFragment = serde_json::from_str(&std::fs::read_to_string(&frag_path)?)?;
        pm.check_schema().map_err(anyhow::Error::msg)?;
        let attr_of = |name: &str| -> Option<bool> {
            pm.items
                .iter()
                .find(|it| it.path.last().is_some_and(|s| s == name))
                .map(|it| it.attrs.iter().any(|a| a == "proc_macro"))
        };
        for entry in ["probe_derive", "probe_bang", "probe_attr"] {
            match attr_of(entry) {
                Some(true) => {
                    ck.passes += 1;
                    println!("PASS  {entry}: attrs carry proc_macro");
                }
                Some(false) => ck
                    .failures
                    .push(format!("[{entry}] attrs must carry proc_macro")),
                None => ck.failures.push(format!("[{entry}] item not found")),
            }
        }
        // Negative control: a plain (private — proc-macro crates can export
        // nothing else) fn must not carry the attr. Absent-from-fragment is
        // fine too; wrongly attributed is the only failure.
        match attr_of("plain_helper") {
            Some(true) => ck
                .failures
                .push("[plain_helper] must NOT carry proc_macro (per-item root)".into()),
            _ => {
                ck.passes += 1;
                println!("PASS  plain_helper: no proc_macro attr");
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
