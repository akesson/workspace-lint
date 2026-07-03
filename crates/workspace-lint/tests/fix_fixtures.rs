//! Fixture-pair tests for `workspace-lint --fix`.
//!
//! Each fixture under `tests/fixtures/fix__<name>/` has two sibling trees:
//! - `input/` — the workspace state before `--fix` runs.
//! - `expected/` — what the tree should look like after `--fix`.
//!
//! On `cargo test`, the driver copies `input/` to a tempdir, runs the
//! binary with `--fix`, and asserts the resulting tree equals `expected/`
//! byte-for-byte.
//!
//! On `WORKSPACE_LINT_BLESS=1 cargo test`, the driver instead overwrites
//! `expected/` with the post-fix tree. The test still passes so a casual
//! `BLESS=1 cargo test` run does the right thing.

use std::path::{Path, PathBuf};
use tempfile::TempDir;

mod common;
use common::{bless_enabled, copy_tree, walk_files, workspace_lint};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn run_fix_fixture(name: &str) {
    let fixture = manifest_dir().join("tests/fixtures").join(name);
    let input = fixture.join("input");
    let expected = fixture.join("expected");
    assert!(
        input.is_dir(),
        "fixture {name}: missing input/ at {}",
        input.display()
    );
    if !bless_enabled() {
        assert!(
            expected.is_dir(),
            "fixture {name}: missing expected/ at {} \
             (run `WORKSPACE_LINT_BLESS=1 cargo test {name}` to generate)",
            expected.display()
        );
    }

    let tmp = TempDir::new().expect("create tempdir");
    copy_tree(&input, tmp.path()).expect("copy input → tempdir");

    // --fix runs the renderer after fixing, which exits 1 if any Deny-level
    // diagnostic survived. Fixture tests focus on the resulting tree, not
    // exit status, so the assertion is dropped here.
    let mut cmd = workspace_lint();
    cmd.current_dir(tmp.path()).arg("--fix");
    let _ = cmd.assert();

    if bless_enabled() {
        sync_tree(tmp.path(), &expected).expect("bless expected/");
        eprintln!("blessed: {}", expected.display());
    } else {
        assert_trees_equal(tmp.path(), &expected);
    }
}

/// Wholesale replace `dst` with `src`'s comparison-relevant files — the same set
/// [`assert_trees_equal`] checks, excluding the `Cargo.lock` that
/// `cargo metadata` generates as a side-effect (no fixture commits one, so it
/// must not leak into a blessed `expected/`). Deletes any pre-existing `dst`
/// first so removals propagate through bless.
fn sync_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    if dst.is_dir() {
        std::fs::remove_dir_all(dst)?;
    }
    for rel in walk_files(src, &["Cargo.lock"]) {
        let to = dst.join(&rel);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(src.join(&rel), &to)?;
    }
    Ok(())
}

fn assert_trees_equal(actual: &Path, expected: &Path) {
    // `walk_files` returns relative, sorted paths (and excludes the generated
    // `Cargo.lock`), so the two lists compare directly.
    let actual_files = walk_files(actual, &["Cargo.lock"]);
    let expected_files = walk_files(expected, &["Cargo.lock"]);

    assert_eq!(
        actual_files, expected_files,
        "tree contents differ.\n  actual:   {actual_files:#?}\n  expected: {expected_files:#?}\n\
         (run `WORKSPACE_LINT_BLESS=1 cargo test` to regenerate expected/)"
    );

    for rel in actual_files {
        let a = std::fs::read_to_string(actual.join(&rel))
            .unwrap_or_else(|e| panic!("read actual {}: {e}", rel.display()));
        let e = std::fs::read_to_string(expected.join(&rel))
            .unwrap_or_else(|err| panic!("read expected {}: {err}", rel.display()));
        assert_eq!(
            a,
            e,
            "file content differs at {}\n--- actual ---\n{a}\n--- expected ---\n{e}\n\
             (run `WORKSPACE_LINT_BLESS=1 cargo test` to update)",
            rel.display()
        );
    }
}

// --- One test per fixture below. New fixtures go in
//     tests/fixtures/fix__<name>/{input,expected}/ then get a #[test]
//     wrapper here. The `every_fixturable_lint_has_a_fix_fixture` guard in
//     src/lints/lints_id.rs (its `#[cfg(test)] mod tests`) verifies the
//     FIXTURABLE_LINTS list stays in sync with what exists on disk.

#[test]
fn fix_centralized_deps() {
    run_fix_fixture("fix__centralized_deps");
}

#[test]
fn fix_unused_deps() {
    run_fix_fixture("fix__unused_deps");
}

#[test]
fn fix_unused_pub() {
    run_fix_fixture("fix__unused_pub");
}
