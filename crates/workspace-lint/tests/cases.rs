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
use std::process::Command;
use std::time::{Duration, SystemTime};
use tempfile::TempDir;

mod common;
use common::{bless_enabled, copy_tree, normalize_stderr, workspace_lint};

/// Apply an optional `setup.toml` (sibling of `workspace/`) to the copied
/// tempdir before the binary runs. Lets cases that need state which can't be
/// committed inert — a git index, or relative file mtimes — join the standard
/// taxonomy. Schema:
///
/// ```toml
/// [git]                       # for stale-git-index
/// init = true                 # git init + add -A + commit
/// delete_after = ["a/b.rs"]   # rm from disk AFTER commit (stays in the index)
///
/// [[mtime]]                   # for freshness; relative order is deterministic
/// path = "crates/api/CLAUDE.md"
/// order = 0                   # lower = older
/// ```
fn apply_setup(case_dir: &Path, tmp: &Path) -> Result<(), String> {
    let setup_path = case_dir.join("setup.toml");
    if !setup_path.exists() {
        return Ok(());
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

    Ok(())
}

fn git_cmd(dir: &Path, args: &[&str]) -> Result<(), String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
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

#[derive(Debug, Clone, Copy)]
enum Kind {
    TruePositive,
    TrueNegative,
    KnownFalsePositive,
    KnownFalseNegative,
}

impl Kind {
    fn from_dir_name(name: &str) -> Option<Self> {
        match name {
            "true_positives" => Some(Self::TruePositive),
            "true_negatives" => Some(Self::TrueNegative),
            "known_false_positives" => Some(Self::KnownFalsePositive),
            "known_false_negatives" => Some(Self::KnownFalseNegative),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::TruePositive => "TP",
            Self::TrueNegative => "TN",
            Self::KnownFalsePositive => "KFP",
            Self::KnownFalseNegative => "KFN",
        }
    }

    fn expects_failure_exit(self) -> bool {
        matches!(self, Self::TruePositive | Self::KnownFalsePositive)
    }
}

fn cases_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("cases")
}

struct Failure {
    case_path: PathBuf,
    kind: Kind,
    reason: String,
}

fn run_case(lint: &str, kind: Kind, case_dir: &Path, bless: bool) -> Result<(), Failure> {
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
    apply_setup(case_dir, tmp.path()).map_err(|e| Failure {
        case_path: case_dir.to_path_buf(),
        kind,
        reason: format!("setup: {e}"),
    })?;

    let output = workspace_lint()
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

    let stderr_raw = String::from_utf8_lossy(&output.stderr).into_owned();
    let stderr = normalize_stderr(&stderr_raw, tmp.path());

    if bless {
        std::fs::write(&expected_path, &stderr).map_err(|e| Failure {
            case_path: case_dir.to_path_buf(),
            kind,
            reason: format!("bless write: {e}"),
        })?;
        let _ = lint;
        return Ok(());
    }

    let expected = std::fs::read_to_string(&expected_path)
        .unwrap_or_default()
        .replace("\r\n", "\n");
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

    if expected.trim() != stderr.trim() {
        return Err(Failure {
            case_path: case_dir.to_path_buf(),
            kind,
            reason: format!(
                "stderr mismatch ({}). Run with WORKSPACE_LINT_BLESS=1 to update.\n\
                 expected:\n{}\n---\nactual:\n{}",
                kind.label(),
                expected,
                stderr,
            ),
        });
    }

    Ok(())
}

fn walk_cases(mut visit: impl FnMut(&str, Kind, &Path)) {
    let root = cases_root();
    if !root.exists() {
        return;
    }
    for lint_entry in std::fs::read_dir(&root).expect("read cases/").flatten() {
        let lint_dir = lint_entry.path();
        if !lint_dir.is_dir() {
            continue;
        }
        let lint = lint_dir.file_name().and_then(|s| s.to_str()).unwrap_or("");
        for kind_entry in std::fs::read_dir(&lint_dir).expect("read kind/").flatten() {
            let kind_dir = kind_entry.path();
            let Some(kind_name) = kind_dir.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            let Some(kind) = Kind::from_dir_name(kind_name) else {
                continue;
            };
            for case_entry in std::fs::read_dir(&kind_dir).expect("read case/").flatten() {
                let case_dir = case_entry.path();
                if case_dir.is_dir() {
                    visit(lint, kind, &case_dir);
                }
            }
        }
    }
}

#[test]
fn cases_pass_or_track_known_limitations() {
    let bless = bless_enabled();
    let mut failures: Vec<Failure> = Vec::new();
    let mut count = 0;
    walk_cases(|lint, kind, case_dir| {
        count += 1;
        if let Err(err) = run_case(lint, kind, case_dir, bless) {
            failures.push(err);
        }
    });

    if bless {
        eprintln!("Blessed {count} case(s)");
        return;
    }

    if count == 0 {
        // Don't fail the test when no cases are defined yet; this lets the
        // test file exist before any fixtures are added. Promote to an
        // assertion once the directory is meant to be populated.
        eprintln!("No cases found under tests/cases/");
        return;
    }

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
