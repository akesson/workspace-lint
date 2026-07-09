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
use tempfile::TempDir;

mod common;
use common::{
    Kind, SnapshotResult, apply_setup, bless_enabled, cases_root, copy_tree, snapshot_stderr,
    walk_cases, workspace_lint,
};

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

    // Optional per-case setup: state that can't live inert in a committed
    // fixture — e.g. the git repo `stale-git-index` needs. See `apply_setup`.
    let args = apply_setup(case_dir, tmp.path()).map_err(|e| Failure {
        case_path: case_dir.to_path_buf(),
        kind,
        reason: format!("setup: {e}"),
    })?;

    let output = workspace_lint()
        .args(&args)
        .current_dir(tmp.path())
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
