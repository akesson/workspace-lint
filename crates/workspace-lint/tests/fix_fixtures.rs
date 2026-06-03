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

use assert_cmd::cargo::cargo_bin_cmd;
use fs_err as fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn workspace_lint() -> assert_cmd::Command {
    cargo_bin_cmd!("workspace-lint")
}

fn bless_enabled() -> bool {
    std::env::var("WORKSPACE_LINT_BLESS").is_ok()
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
    let _ = workspace_lint()
        .current_dir(tmp.path())
        .arg("--fix")
        .assert();

    if bless_enabled() {
        sync_tree(tmp.path(), &expected).expect("bless expected/");
        eprintln!("blessed: {}", expected.display());
    } else {
        assert_trees_equal(tmp.path(), &expected);
    }
}

fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    for entry in walk_files(src) {
        let rel = entry.strip_prefix(src).expect("strip prefix");
        let target = dst.join(rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&entry, &target)?;
    }
    Ok(())
}

/// Wholesale replace `dst` with the contents of `src`. Deletes any
/// pre-existing files under `dst` so removals propagate through bless.
fn sync_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    if dst.is_dir() {
        fs::remove_dir_all(dst)?;
    }
    fs::create_dir_all(dst)?;
    copy_tree(src, dst)
}

fn assert_trees_equal(actual: &Path, expected: &Path) {
    let actual_files: Vec<PathBuf> = walk_files(actual)
        .into_iter()
        .map(|p| p.strip_prefix(actual).unwrap().to_path_buf())
        .collect();
    let expected_files: Vec<PathBuf> = walk_files(expected)
        .into_iter()
        .map(|p| p.strip_prefix(expected).unwrap().to_path_buf())
        .collect();

    let mut actual_sorted = actual_files.clone();
    let mut expected_sorted = expected_files.clone();
    actual_sorted.sort();
    expected_sorted.sort();

    assert_eq!(
        actual_sorted, expected_sorted,
        "tree contents differ.\n  actual:   {actual_sorted:#?}\n  expected: {expected_sorted:#?}\n\
         (run `WORKSPACE_LINT_BLESS=1 cargo test` to regenerate expected/)"
    );

    for rel in actual_sorted {
        let a = fs::read_to_string(actual.join(&rel))
            .unwrap_or_else(|e| panic!("read actual {}: {e}", rel.display()));
        let e = fs::read_to_string(expected.join(&rel))
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

/// Recursively walk a directory and return every regular file path.
///
/// Filters out `Cargo.lock` and `target/` since some lints (resolver-backed
/// ones) shell out to `cargo metadata`, which can create those as a side
/// effect. They're not part of the user-visible workspace state we're
/// asserting on.
fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if !root.is_dir() {
        return out;
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if path.is_dir() {
                if name == "target" {
                    continue;
                }
                stack.push(path);
            } else if path.is_file() {
                if name == "Cargo.lock" {
                    continue;
                }
                out.push(path);
            }
        }
    }
    out
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
