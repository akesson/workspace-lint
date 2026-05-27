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

use assert_cmd::cargo::cargo_bin_cmd;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

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

fn workspace_lint() -> assert_cmd::Command {
    cargo_bin_cmd!("workspace-lint")
}

fn bless_enabled() -> bool {
    std::env::var_os("WORKSPACE_LINT_BLESS").is_some()
}

fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    if !src.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("source missing: {}", src.display()),
        ));
    }
    for entry in walkdir(src) {
        let entry = entry?;
        let rel = entry.strip_prefix(src).unwrap();
        let target = dst.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&target)?;
        } else {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&entry, &target)?;
        }
    }
    Ok(())
}

fn walkdir(root: &Path) -> impl Iterator<Item = std::io::Result<PathBuf>> + use<> {
    // Minimal recursive iterator: avoids pulling in walkdir crate.
    let mut stack = vec![root.to_path_buf()];
    std::iter::from_fn(move || {
        let path = stack.pop()?;
        if path.is_dir() {
            let read = match std::fs::read_dir(&path) {
                Ok(r) => r,
                Err(e) => return Some(Err(e)),
            };
            for entry in read.flatten() {
                stack.push(entry.path());
            }
        }
        Some(Ok(path))
    })
}

fn normalize_stderr(stderr: &str, tmp: &Path) -> String {
    // Build every reasonable spelling of the tempdir path:
    //
    // - `tmp.path()` and `tmp.canonicalize()` to handle macOS' /var → /private/var
    //   symlink dance, and the short-vs-long-name distinction on Windows
    //   (`RUNNER~1` vs `runneradmin`).
    // - Forward-slash forms of both, since the renderer normalizes paths to
    //   forward-slash on Windows but `Path::to_string_lossy()` still gives us
    //   backslashes here.
    // - Verbatim-prefix-stripped (`\\?\`) variants in case `canonicalize` ever
    //   returns one (currently it doesn't reach our stderr — the renderer
    //   strips it — but defending against it costs nothing).
    //
    // Sort by length descending and replace in that order: longer paths
    // (e.g. /private/var/folders/...) must consume their content before the
    // shorter alias (/var/folders/...) gets a chance, otherwise we leave a
    // stray prefix behind.
    let mut spellings: Vec<String> = Vec::new();
    let push = |spellings: &mut Vec<String>, s: String| {
        if !s.is_empty() && !spellings.contains(&s) {
            spellings.push(s);
        }
    };
    push(&mut spellings, tmp.to_string_lossy().into_owned());
    if let Ok(canon) = tmp.canonicalize() {
        push(&mut spellings, canon.to_string_lossy().into_owned());
    }
    let with_fs: Vec<String> = spellings.iter().map(|s| s.replace('\\', "/")).collect();
    for s in with_fs {
        push(&mut spellings, s);
    }
    let stripped: Vec<String> = spellings
        .iter()
        .filter_map(|s| s.strip_prefix(r"\\?\").map(|t| t.to_string()))
        .collect();
    for s in stripped {
        push(&mut spellings, s);
    }
    spellings.sort_by_key(|s| std::cmp::Reverse(s.len()));

    let mut out = stderr.to_string();
    for s in &spellings {
        out = out.replace(s.as_str(), "<TMP>");
    }
    out
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

    let output = workspace_lint()
        .current_dir(tmp.path())
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

    let expected = std::fs::read_to_string(&expected_path).unwrap_or_default();
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
