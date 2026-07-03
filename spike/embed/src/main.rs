//! Single-bin embed check — SPIKE-rustc-fidelity-tree.md §12.10.
//!
//! Replaces the `cargo dylint --lib-path <LIB> --no-deps -- -p <PKG>` CLI call
//! with a direct `dylint::run(&opts)` from a *stable* binary. If this produces
//! the same IR fragment the CLI did, the single-binary packaging (§3) holds:
//! `workspace-lint` can embed the `dylint` orchestrator rather than shelling out
//! to `cargo-dylint`.
//!
//! Usage: wl-embed <TARGET_WORKSPACE_DIR> <LIB_PATH> <WL_IR_OUT> [PKG..] [-- CARGO_ARGS..]
//!
//! With no `PKG` (or the `*` sentinel) it checks the **whole workspace** — the
//! multi-crate orchestration path (SPIKE §4/§5): `dylint::run` sets no `-p`, so
//! cargo fans out over every default workspace member and the driver emits one
//! `IrFragment` per crate. `--no-deps` keeps that to workspace members (registry
//! deps compile normally but get no lint pass). We never loop crates ourselves;
//! the barrier is cargo finishing the last crate. Naming one or more packages
//! keeps the original single-crate behaviour.
//!
//! Everything after a `--` is forwarded verbatim to the underlying `cargo check`
//! (`Check.args`) — the load-bearing cfg selector (SPIKE §7): cfg-stripping runs
//! in the compiler frontend *before* the driver sees `TyCtxt`, so one compile =
//! one config. `-- --tests`, `-- --all-features`, `-- --no-default-features
//! --features x` each select a different config; point `WL_IR_OUT` at a distinct
//! dir per config and the Phase-2 assembler unions them (`(crate, def_path_str)`
//! join — `DefPathHash` is *not* cross-config stable). This is requirement (c)
//! below, previously wired-but-unused.
//!
//! Proves the four §12.10 requirements are all reachable via the opts struct:
//!   (a) target a workspace     → set CWD to it (dylint checks the CWD workspace)
//!   (b) load a specific lib     → LibrarySelection.lib_paths
//!   (c) pass cargo args/features→ Check.args (unused here; wired + documented)
//!   (d) thread env (WL_IR_OUT)  → set on this process; the driver inherits it

use std::collections::BTreeSet;
use std::path::Path;

use dylint::opts::{Check, Dylint, LibrarySelection, Operation};

fn main() -> anyhow::Result<()> {
    let mut argv = std::env::args().skip(1);
    let target = argv.next().expect("arg1: target workspace dir");
    let lib_path = argv.next().expect("arg2: lib path (…@toolchain.dylib)");
    let ir_out = argv.next().expect("arg3: WL_IR_OUT dir");
    // We `chdir` into `target` below (dylint checks the CWD workspace), so any
    // relative path arg would resolve against the wrong dir afterwards — dylint's
    // `--path <lib>` lookup, `force_relint`'s mtime bump, and the driver's inherited
    // `WL_IR_OUT` all run post-chdir. Resolve to absolute now, while CWD is still
    // the caller's. (`target` is consumed before the chdir, so it stays as-is.)
    let lib_path = std::path::absolute(&lib_path)?.to_string_lossy().into_owned();
    let ir_out = std::path::absolute(&ir_out)?.to_string_lossy().into_owned();
    // arg4.. : zero or more packages, then optionally `--` + cargo check args.
    // Empty package list (or a lone `*`) ⇒ whole workspace; args after `--` are
    // the cfg selector forwarded to `cargo check` (see the module docs).
    let mut packages: Vec<String> = Vec::new();
    let mut cargo_args: Vec<String> = Vec::new();
    let mut past_sep = false;
    for a in argv {
        if past_sep {
            cargo_args.push(a);
        } else if a == "--" {
            past_sep = true;
        } else if a != "*" {
            packages.push(a);
        }
    }

    // Which fragments a *complete* run must produce — computed from cargo
    // metadata BEFORE the chdir (it takes an explicit manifest path). `None`
    // means "guard skipped" (an unmodeled target-selection flag); the run still
    // proceeds, just without the completeness check.
    let expected = expected_fragments(&target, &packages, &cargo_args)?;

    // (d) env the spawned driver inherits — same mechanism the CLI run used.
    unsafe { std::env::set_var("WL_IR_OUT", &ir_out) };
    // (a) dylint checks the workspace in the current directory.
    std::env::set_current_dir(&target)?;

    let scope = if packages.is_empty() {
        "whole workspace".to_string()
    } else {
        format!("-p {}", packages.join(" -p "))
    };
    let cfg = if cargo_args.is_empty() {
        "default cfg".to_string()
    } else {
        format!("cfg: cargo check {}", cargo_args.join(" "))
    };

    let opts = Dylint {
        pipe_stderr: None,
        pipe_stdout: None,
        quiet: false,
        operation: Operation::Check(Check {
            lib_sel: LibrarySelection {
                // (b) load exactly our lint dylib by path. Cloned so the
                // completeness guard can bump its mtime on a re-lint.
                lib_paths: vec![lib_path.clone()],
                ..Default::default()
            },
            no_deps: true, // --no-deps: workspace members only, never deps
            packages,      // empty ⇒ all default members (cargo fans out)
            // (c) cargo check args forwarded from after `--` — the cfg selector
            //     (`--tests` / `--all-features` / `--no-default-features …`). One
            //     compile per config; the assembler unions the per-config IR dirs.
            args: cargo_args,
            ..Default::default()
        }),
    };

    eprintln!("wl-embed: calling dylint::run() over {scope} [{cfg}] (no cargo-dylint CLI)…");
    dylint::run(&opts)?;
    eprintln!("wl-embed: dylint::run() returned Ok");

    // Completeness guard (SPIKE §11 caching gotcha). `WL_IR_OUT` is not in cargo's
    // fingerprint, so a crate that's up-to-date is *not* recompiled and its lint
    // pass never runs — no fragment is (re)written and `dylint::run` still returns
    // Ok. A fresh crate's *existing* fragment is still valid (its inputs are
    // unchanged), so this is a pure existence check. On a miss we force a re-lint
    // by bumping the lint dylib's mtime — dylint fingerprints the dylib into every
    // workspace-member unit's dep-info, so cargo re-checks members (not registry
    // deps) — and run exactly once more.
    if let Some(expected) = expected {
        let ir_dir = Path::new(&ir_out);
        let missing = missing_fragments(ir_dir, &expected);
        if missing.is_empty() {
            eprintln!(
                "wl-embed: completeness check OK ({} fragment(s))",
                expected.len()
            );
        } else {
            eprintln!(
                "wl-embed: {} expected fragment(s) missing (cargo freshness skipped their lint \
                 pass): {missing:?} — bumping lint-dylib mtime and re-running once",
                missing.len()
            );
            force_relint(&lib_path)?;
            dylint::run(&opts)?;
            let still = missing_fragments(ir_dir, &expected);
            anyhow::ensure!(
                still.is_empty(),
                "fragments still missing after forced re-lint: {still:?} (expected {expected:?} in {ir_out})"
            );
            eprintln!(
                "wl-embed: completeness restored ({} fragment(s) regenerated)",
                missing.len()
            );
        }
    }
    Ok(())
}

/// The set of IR fragment filenames a complete run must produce, keyed exactly
/// as the extractor's `write_fragment` names them (`<crate>.json`, or
/// `<crate>+test.json` when compiled with `--tests` — `sess.opts.test`). Crate
/// name = the cargo *target* name with `-` → `_`. Returns `Ok(None)` (guard
/// skipped, with a warning) when `cargo_args` carries a target-selection flag we
/// don't model, so the guard never fires spuriously.
fn expected_fragments(
    manifest_dir: &str,
    packages: &[String],
    cargo_args: &[String],
) -> anyhow::Result<Option<BTreeSet<String>>> {
    // Flags that change which *targets* compile, beyond the `--tests` we model.
    // (Feature flags — `--features`, `--all-features`, `--no-default-features` —
    // change cfg/content, not the target set, so they're fine.)
    const UNMODELED: &[&str] = &[
        "--lib",
        "--bins",
        "--bin",
        "--examples",
        "--example",
        "--benches",
        "--bench",
        "--test",
        "--all-targets",
        "--doc",
        "-p",
        "--package",
        "--workspace",
        "--exclude",
    ];
    if let Some(flag) = cargo_args.iter().find(|a| UNMODELED.contains(&a.as_str())) {
        eprintln!(
            "wl-embed: completeness guard skipped — unmodeled target-selection flag `{flag}`"
        );
        return Ok(None);
    }
    let tests = cargo_args.iter().any(|a| a == "--tests");

    let md = cargo_metadata::MetadataCommand::new()
        .manifest_path(format!("{}/Cargo.toml", manifest_dir.trim_end_matches('/')))
        .no_deps()
        .exec()?;
    let member_ids: BTreeSet<String> = md
        .workspace_members
        .iter()
        .map(|id| id.to_string())
        .collect();
    let want_pkg = |name: &str| packages.is_empty() || packages.iter().any(|p| p == name);

    let mut expected = BTreeSet::new();
    for p in &md.packages {
        if !member_ids.contains(&p.id.to_string()) || !want_pkg(p.name.as_str()) {
            continue;
        }
        for t in &p.targets {
            let mut name = t.name.replace('-', "_");
            // Compare kinds via Display (as wl-assemble does) so we don't couple
            // to cargo_metadata's enum representation.
            let mut is_compile_unit = false; // lib/bin/proc-macro (linted primary)
            let mut is_test_target = false; // integration test (only under --tests)
            for k in &t.kind {
                match k.to_string().as_str() {
                    "lib" | "rlib" | "dylib" | "cdylib" | "staticlib" | "proc-macro" => {
                        is_compile_unit = true
                    }
                    // Bins carry the extractor's `@bin` infix (a package's bin
                    // may share the lib's crate name) — sync with
                    // `wl-engine::orchestrate::guard`.
                    "bin" => {
                        is_compile_unit = true;
                        name = format!("{name}@bin");
                    }
                    "test" => is_test_target = true,
                    _ => {} // example/bench/custom-build — not compiled by check/--tests here
                }
            }
            if tests {
                // `--tests` builds unit-test harnesses for lib/bin/proc-macro AND
                // integration tests, all with `sess.opts.test` ⇒ `+test` suffix.
                if is_compile_unit || is_test_target {
                    expected.insert(format!("{name}+test.json"));
                }
            } else if is_compile_unit {
                // Default `check`: lib/bin/proc-macro only (no test/example/bench).
                expected.insert(format!("{name}.json"));
            }
        }
    }
    Ok(Some(expected))
}

/// Expected fragment filenames not present in `ir_out`.
fn missing_fragments(ir_out: &Path, expected: &BTreeSet<String>) -> Vec<String> {
    expected
        .iter()
        .filter(|name| !ir_out.join(name).exists())
        .cloned()
        .collect()
}

/// Force the next `dylint::run` to re-lint every workspace member by bumping the
/// lint dylib's mtime. Verified mechanism (SPIKE §11): dylint's driver inserts
/// the dylib path into each primary-package unit's dep-info, so cargo treats a
/// newer dylib as a changed input for members only — deps stay fresh.
fn force_relint(lib_path: &str) -> anyhow::Result<()> {
    let f = std::fs::OpenOptions::new().append(true).open(lib_path)?;
    f.set_modified(std::time::SystemTime::now())?;
    Ok(())
}
