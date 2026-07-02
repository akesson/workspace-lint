//! Flag files that are still tracked by git but no longer exist on disk.
//!
//! Always-on (no config gate): runs `git ls-files` once and reports every
//! tracked path that's missing. The lint lives in the registry as a
//! zero-argument `StaleGitIndex::new()` because there's nothing user-tunable
//! about it.

use std::path::Path;

use crate::diagnostic::Diagnostic;
use crate::diagnostic::builder::at_workspace;
use crate::lints::{Lint, LintContext, LintId};

#[cfg(test)]
mod tests;

pub(crate) struct StaleGitIndex;

impl StaleGitIndex {
    pub fn new() -> Self {
        Self
    }
}

impl Default for StaleGitIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl Lint for StaleGitIndex {
    fn id(&self) -> LintId {
        LintId::StaleGitIndex
    }

    fn check(&self, _cx: &LintContext<'_>) -> Vec<Diagnostic> {
        check()
    }
}

pub(crate) fn check() -> Vec<Diagnostic> {
    check_in(Path::new("."))
}

/// Run `git ls-files -z` in `base` and report tracked-but-missing paths.
///
/// Best-effort and always-on: a spawn failure (git not installed) or a
/// non-git directory yields *no* findings rather than aborting the run — the
/// old `std::process::exit(1)` would take every other lint's output down with
/// it. `-z` gives NUL-separated paths with no quoting, so non-ASCII / spaced
/// names are handled verbatim (git otherwise C-quotes them by default).
fn check_in(base: &Path) -> Vec<Diagnostic> {
    let Ok(output) = crate::git::command(base).args(["ls-files", "-z"]).output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let listing = String::from_utf8_lossy(&output.stdout);
    build_diagnostics(&missing_paths(&listing, base))
}

/// Keep only the tracked paths from a NUL-separated `git ls-files -z` listing
/// that no longer exist on disk (resolved relative to `base`).
fn missing_paths<'a>(ls_files_z: &'a str, base: &Path) -> Vec<&'a str> {
    ls_files_z
        .split('\0')
        .filter(|p| !p.is_empty())
        .filter(|p| !base.join(p).exists())
        .collect()
}

fn build_diagnostics(paths: &[&str]) -> Vec<Diagnostic> {
    let lint_id = LintId::StaleGitIndex.id();
    paths
        .iter()
        .map(|path| {
            at_workspace(
                lint_id,
                format!("deleted file `{path}` is still tracked by git"),
            )
            .help(format!("run `git rm {path}` to stage the removal"))
            .build()
        })
        .collect()
}
