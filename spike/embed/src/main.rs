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

use dylint::opts::{Check, Dylint, LibrarySelection, Operation};

fn main() -> anyhow::Result<()> {
    let mut argv = std::env::args().skip(1);
    let target = argv.next().expect("arg1: target workspace dir");
    let lib_path = argv.next().expect("arg2: lib path (…@toolchain.dylib)");
    let ir_out = argv.next().expect("arg3: WL_IR_OUT dir");
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
                // (b) load exactly our lint dylib by path.
                lib_paths: vec![lib_path],
                ..Default::default()
            },
            no_deps: true,     // --no-deps: workspace members only, never deps
            packages,          // empty ⇒ all default members (cargo fans out)
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
    Ok(())
}
