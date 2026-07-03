//! Fixture-based test harness with explicit four-kind outcome taxonomy.
//!
//! Layout under `tests/cases/`:
//!
//! ```text
//! cases/
//!   <lint_name>/
//!     true_positives/      lint correctly flags
//!     true_negatives/      lint correctly passes
//!     known_false_positives/  lint incorrectly flags - regression-tracked
//!     known_false_negatives/  lint incorrectly passes - regression-tracked
//! ```
//!
//! Each `<case_name>/` directory holds a `workspace/` subtree (copied to a
//! tempdir before each run) and an `expected.stderr` snapshot. Snapshots are
//! path-normalized: the tempdir prefix is replaced by `<TMP>` so the
//! expected file is stable across machines and runs.
//!
//! The taxonomy gives both real findings and known limitations a forcing
//! function: if a `known_false_positives/` case ever stops firing, the test
//! fails - signalling the case should be promoted to `true_positives/` or
//! deleted. If a `known_false_negatives/` case ever starts firing, same
//! signal in the opposite direction.
//!
//! ## Blessing
//!
//! Regenerate `expected.stderr` for every case:
//!
//! ```text
//! WORKSPACE_LINT_BLESS=1 cargo test --test cases
//! ```

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tempfile::TempDir;

mod common;
use common::{
    Kind, SnapshotResult, bless_enabled, cases_root, copy_tree, snapshot_stderr, walk_cases,
    workspace_lint,
};

/// Apply an optional `setup.toml` (sibling of `workspace/`) to the copied
/// tempdir before the binary runs. Lets cases that need state which can't be
/// committed inert — a git index, or relative file mtimes — join the standard
/// taxonomy. Returns extra CLI args for the case's binary invocation. Schema:
///
/// ```toml
/// args = ["--fast-only"]      # extra CLI args for the binary
///
/// [git]                       # for stale-git-index
/// init = true                 # git init + add -A + commit
/// delete_after = ["a/b.rs"]   # rm from disk AFTER commit (stays in the index)
///
/// [[mtime]]                   # for freshness; relative order is deterministic
/// path = "crates/api/CLAUDE.md"
/// order = 0                   # lower = older
/// ```
fn apply_setup(case_dir: &Path, tmp: &Path) -> Result<Vec<String>, String> {
    let setup_path = case_dir.join("setup.toml");
    if !setup_path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&setup_path).map_err(|e| format!("read setup.toml: {e}"))?;
    let doc: toml::Value = toml::from_str(&text).map_err(|e| format!("parse setup.toml: {e}"))?;

    if let Some(git) = doc.get("git") {
        if git.get("init").and_then(toml::Value::as_bool) == Some(true) {
            git_cmd(tmp, &["init", "-q"])?;
            git_cmd(tmp, &["add", "-A"])?;
            git_cmd(
                tmp,
                &[
                    "-c",
                    "user.name=test",
                    "-c",
                    "user.email=test@example.com",
                    "commit",
                    "-q",
                    "-m",
                    "setup",
                ],
            )?;
        }
        if let Some(deletes) = git.get("delete_after").and_then(toml::Value::as_array) {
            for entry in deletes {
                let rel = entry
                    .as_str()
                    .ok_or("delete_after entries must be strings")?;
                std::fs::remove_file(tmp.join(rel))
                    .map_err(|e| format!("delete_after {rel}: {e}"))?;
            }
        }
    }

    // Append text to a file *after* copy — used to inject a `# workspace-lint:`
    // directive that must reach the case run but stay out of the committed
    // fixture (otherwise this repo's own dogfood scan would pick the directive
    // up from tests/cases/ and trip stale-expect / unknown-lint).
    if let Some(entries) = doc.get("append").and_then(toml::Value::as_array) {
        for entry in entries {
            let rel = entry
                .get("path")
                .and_then(toml::Value::as_str)
                .ok_or("append entry needs a string `path`")?;
            let text = entry
                .get("text")
                .and_then(toml::Value::as_str)
                .ok_or("append entry needs a string `text`")?;
            let p = tmp.join(rel);
            let mut content =
                std::fs::read_to_string(&p).map_err(|e| format!("append read {rel}: {e}"))?;
            content.push_str(text);
            std::fs::write(&p, content).map_err(|e| format!("append write {rel}: {e}"))?;
        }
    }

    // Extra CLI args for the binary — lets a case exercise a flagged run
    // (e.g. `--fast-only`) while staying in the standard taxonomy.
    let mut args = Vec::new();
    if let Some(entries) = doc.get("args").and_then(toml::Value::as_array) {
        for entry in entries {
            args.push(
                entry
                    .as_str()
                    .ok_or("args entries must be strings")?
                    .to_string(),
            );
        }
    }

    if let Some(entries) = doc.get("mtime").and_then(toml::Value::as_array) {
        // Assign mtimes in `order`: a deterministic base plus 10s per step, so
        // a lower order is strictly older regardless of filesystem resolution.
        let base = SystemTime::now();
        for entry in entries {
            let rel = entry
                .get("path")
                .and_then(toml::Value::as_str)
                .ok_or("mtime entry needs a string `path`")?;
            let order = entry
                .get("order")
                .and_then(toml::Value::as_integer)
                .ok_or("mtime entry needs an integer `order`")?;
            let when = base + Duration::from_secs(order.max(0) as u64 * 10);
            let f = std::fs::File::options()
                .write(true)
                .open(tmp.join(rel))
                .map_err(|e| format!("open {rel} for mtime: {e}"))?;
            f.set_times(std::fs::FileTimes::new().set_modified(when))
                .map_err(|e| format!("set mtime {rel}: {e}"))?;
        }
    }

    Ok(args)
}

fn git_cmd(dir: &Path, args: &[&str]) -> Result<(), String> {
    let out = common::git(dir)
        .args(args)
        .output()
        .map_err(|e| format!("git {args:?}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

struct Failure {
    case_path: PathBuf,
    kind: Kind,
    reason: String,
}

fn run_case(kind: Kind, case_dir: &Path, bless: bool) -> Result<(), Failure> {
    let workspace_src = case_dir.join("workspace");
    if !workspace_src.exists() {
        return Err(Failure {
            case_path: case_dir.to_path_buf(),
            kind,
            reason: format!("missing workspace/ at {}", workspace_src.display()),
        });
    }
    let expected_path = case_dir.join("expected.stderr");

    let tmp = TempDir::new().map_err(|e| Failure {
        case_path: case_dir.to_path_buf(),
        kind,
        reason: format!("tempdir: {e}"),
    })?;

    copy_tree(&workspace_src, tmp.path()).map_err(|e| Failure {
        case_path: case_dir.to_path_buf(),
        kind,
        reason: format!("copy: {e}"),
    })?;

    // Optional per-case setup: initialize a git repo and/or set relative file
    // mtimes that can't live inert in a committed fixture (they're needed by
    // stale-git-index and freshness). See `apply_setup`.
    let args = apply_setup(case_dir, tmp.path()).map_err(|e| Failure {
        case_path: case_dir.to_path_buf(),
        kind,
        reason: format!("setup: {e}"),
    })?;

    let output = workspace_lint()
        .args(&args)
        .current_dir(tmp.path())
        // Run lints deterministically regardless of the CI env: `freshness`
        // short-circuits when `CI` is set, which would otherwise make its
        // true-positive cases silently pass under CI.
        .env_remove("CI")
        .output()
        .map_err(|e| Failure {
            case_path: case_dir.to_path_buf(),
            kind,
            reason: format!("spawn: {e}"),
        })?;

    let (stderr, snap) = snapshot_stderr(&output.stderr, tmp.path(), &expected_path, bless)
        .map_err(|e| Failure {
            case_path: case_dir.to_path_buf(),
            kind,
            reason: format!("snapshot io: {e}"),
        })?;

    if bless {
        return Ok(());
    }

    // Exit-code policy is cases.rs-specific (corpus_fp doesn't check it), so it
    // stays out of `snapshot_stderr`. Its message embeds the normalized stderr
    // even when the stderr itself matched — hence `snapshot_stderr` hands `stderr`
    // back to us.
    let exit_failure_expected = kind.expects_failure_exit();
    let exit_failure_actual = !output.status.success();
    if exit_failure_expected != exit_failure_actual {
        return Err(Failure {
            case_path: case_dir.to_path_buf(),
            kind,
            reason: format!(
                "exit-code mismatch: expected {} ({}), got {} (exit={:?})\nstderr:\n{stderr}",
                if exit_failure_expected {
                    "failure"
                } else {
                    "success"
                },
                kind.label(),
                if exit_failure_actual {
                    "failure"
                } else {
                    "success"
                },
                output.status.code(),
            ),
        });
    }

    match snap {
        SnapshotResult::Mismatch { expected } => Err(Failure {
            case_path: case_dir.to_path_buf(),
            kind,
            reason: format!(
                "stderr mismatch ({}). Run with WORKSPACE_LINT_BLESS=1 to update.\n\
                 expected:\n{}\n---\nactual:\n{}",
                kind.label(),
                expected,
                stderr,
            ),
        }),
        _ => Ok(()),
    }
}

#[test]
fn cases_pass_or_track_known_limitations() {
    let bless = bless_enabled();
    // Substring filter for focused iteration on one lint or case, e.g.
    // WORKSPACE_LINT_CASE_FILTER=unused-pub. Zero matches still fails the
    // populated-taxonomy assert below — a typo'd filter is loud, not green.
    let filter = std::env::var("WORKSPACE_LINT_CASE_FILTER").ok();
    let mut failures: Vec<Failure> = Vec::new();
    let mut count = 0;
    walk_cases(|_lint, kind, case_dir| {
        if let Some(f) = &filter
            && !case_dir.to_string_lossy().contains(f.as_str())
        {
            return;
        }
        count += 1;
        if let Err(err) = run_case(kind, case_dir, bless) {
            failures.push(err);
        }
    });

    if bless {
        eprintln!("Blessed {count} case(s)");
        return;
    }

    // The taxonomy is populated (100+ committed cases), so zero discovered
    // cases means the discovery walk (or `cases_root()`) is broken — fail loudly
    // rather than green-pass on an empty sweep.
    assert!(
        count > 0,
        "no cases discovered under tests/cases/ — the discovery walk is broken"
    );

    if !failures.is_empty() {
        let report = failures
            .iter()
            .map(|f| {
                format!(
                    "[{}] {}\n{}",
                    f.kind.label(),
                    f.case_path.display(),
                    f.reason
                )
            })
            .collect::<Vec<_>>()
            .join("\n---\n");
        panic!("{} of {count} cases failed:\n{report}", failures.len());
    }
}

/// Drift guard for the semantic-lint routing list (`common::SEMANTIC_LINTS`,
/// consumed today by `fixture_compile.rs`'s offline-compile sweep and by this
/// harness's fast-vs-semantic routing once the rustc-backed ports land).
/// `LintId` lives in the binary crate (no lib target) and can't be imported
/// here, so the tie is transitive: each entry is pinned to its
/// `tests/cases/<lint>/` directory, and lints_id.rs's
/// `every_lint_has_case_fixtures` unit test pins those directory names to
/// `LintId::short()` — a renamed or removed lint breaks this chain instead of
/// silently orphaning the routing list.
#[test]
fn semantic_lint_routing_matches_case_dirs() {
    for lint in common::SEMANTIC_LINTS {
        assert!(
            common::lint_needs_build(lint),
            "lint_needs_build must return true for its own list entry `{lint}`"
        );
        assert!(
            cases_root().join(lint).is_dir(),
            "common::SEMANTIC_LINTS entry `{lint}` has no tests/cases/{lint}/ directory — \
             stale after a lint rename/removal? (directory names are pinned to \
             LintId::short() by lints_id.rs::every_lint_has_case_fixtures)"
        );
    }
}
