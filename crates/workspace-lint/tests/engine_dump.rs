//! Gated end-to-end check of the hidden `--engine-dump` plumbing: spawn the
//! binary against this repository, let it run the full tier (vendored
//! extractor → embedded dylint extraction → Phase-2 assembly), and assert the
//! stats block reaches stdout with exit 0.
//!
//! Gated behind `WL_ENGINE_E2E=1` like `crates/wl-engine/tests/e2e.rs`: it
//! needs the pinned nightly (+ rustc-dev + llvm-tools) and `dylint-link`
//! installed — plain `cargo test --workspace` must stay green on a
//! stable-only machine. Locally:
//!
//! ```sh
//! WL_ENGINE_E2E=1 cargo test -p workspace-lint --test engine_dump -- --nocapture
//! ```

use std::path::PathBuf;

mod common;
use common::workspace_lint;

#[test]
fn engine_dump_prints_stats_and_exits_zero() {
    if std::env::var_os("WL_ENGINE_E2E").is_none() {
        eprintln!("skipped: set WL_ENGINE_E2E=1 (needs the pinned nightly + dylint-link)");
        return;
    }
    // CARGO_MANIFEST_DIR points at crates/workspace-lint/; the workspace root
    // (carrying the dogfood config the run loads) is two levels up.
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("manifest dir has two parents (workspace root)")
        .to_path_buf();

    let assert = workspace_lint()
        .arg("--engine-dump")
        .current_dir(&workspace_root)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();

    // The dogfood config has no `[engine]` table, so the default matrix runs.
    assert!(
        stdout.contains("configs: default"),
        "missing configs line in:\n{stdout}"
    );
    for line in [
        "primary import edges: ",
        "unused-pub union: ",
        "unused-deps: ",
    ] {
        assert!(stdout.contains(line), "missing `{line}` in:\n{stdout}");
    }
}
