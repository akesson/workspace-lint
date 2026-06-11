//! Run workspace-lint against its own repository and assert it passes.
//!
//! This is the load-bearing test for the project's quality bar: any new
//! lint or threshold change must keep this green or come with a paired
//! `expect!` directive that documents the exception. Without it, the tool
//! could regress on its own code and nobody would notice.

use std::path::PathBuf;

mod common;
use common::workspace_lint;

#[test]
fn workspace_lint_runs_clean_on_itself() {
    // CARGO_MANIFEST_DIR points at crates/workspace-lint/. Climb two levels
    // up to reach the workspace root (the path also containing
    // `.workspace-lint.toml`).
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest
        .parent()
        .and_then(|p| p.parent())
        .expect("manifest dir has two parents (workspace root)");
    assert!(
        workspace_root.join(".workspace-lint.toml").exists(),
        "expected dogfood config at {}",
        workspace_root.display()
    );

    workspace_lint()
        .current_dir(workspace_root)
        .assert()
        .success();
}
