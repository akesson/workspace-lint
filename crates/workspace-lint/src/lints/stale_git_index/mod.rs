//! Flag files that are still tracked by git but no longer exist on disk.
//!
//! Always-on (no config gate): runs `git ls-files` once and reports every
//! tracked path that's missing. The lint lives in the registry as a
//! zero-argument `StaleGitIndex::new()` because there's nothing user-tunable
//! about it.

use std::process::Command;

use crate::diagnostic::Diagnostic;
use crate::diagnostic::builder::at_workspace;
use crate::lints::{Lint, LintContext, LintId};

pub struct StaleGitIndex;

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

pub fn check() -> Vec<Diagnostic> {
    let output = Command::new("git")
        .args(["ls-files"])
        .output()
        .unwrap_or_else(|e| {
            eprintln!("failed to run git ls-files: {e}");
            std::process::exit(1);
        });

    let lint_id = LintId::StaleGitIndex.id();
    let files = String::from_utf8_lossy(&output.stdout);
    files
        .lines()
        .filter(|path| !std::path::Path::new(path).exists())
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
