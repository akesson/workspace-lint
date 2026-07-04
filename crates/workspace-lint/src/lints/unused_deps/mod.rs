//! Unused-dependencies check: declared deps vs the crate's actual references.
//!
//! The judged substrate is the rustc-extracted reference graph (the engine's
//! [`wl_engine::SemanticModel`]), facade- and lib-rename-aware via resolved
//! dependency closures, plus the FastModel's syntactic signals (doc-fence
//! refs, feature plumbing).
//!
//! Known limitations (documented in tests/cases/unused-deps/): `build.rs`
//! deps and `*-sys` link-only deps aren't judgeable from references; the
//! `ignore` knob suppresses them.

use std::collections::HashMap;

use crate::lints::{Lint, LintContext, LintId, Requirements};
use wl_diagnostic::Diagnostic;

pub mod config;
mod ir;
#[cfg(test)]
mod tests;

pub(crate) use config::UnusedDepsConfig;

pub(crate) struct UnusedDeps {
    /// Workspace-wide params, used for any crate without a per-crate section.
    global: UnusedDepsConfig,
    /// Per-crate params (keyed by Cargo package name), each *wholesale*
    /// replacing the global config for that crate. Empty for CLI single-check
    /// runs, which have no `[crates.*]` tier.
    per_crate: HashMap<String, UnusedDepsConfig>,
}

impl UnusedDeps {
    pub(crate) fn new(
        global: UnusedDepsConfig,
        per_crate: HashMap<String, UnusedDepsConfig>,
    ) -> Self {
        Self { global, per_crate }
    }

    pub(crate) fn from_cli(ignore: Vec<String>) -> Self {
        Self::new(UnusedDepsConfig { ignore }, HashMap::new())
    }
}

impl Lint for UnusedDeps {
    fn id(&self) -> LintId {
        LintId::UnusedDeps
    }

    fn requirements(&self) -> Requirements {
        Requirements {
            needs_semantic: true,
            // Manifests, doc-fence refs, and workspace-relative paths.
            needs_fast: true,
        }
    }

    fn check(&self, cx: &LintContext<'_>) -> Vec<Diagnostic> {
        let fast = cx.fast.expect("unused-deps requires the FastModel");
        // The runner skips the tier for a memberless workspace (there is
        // nothing to extract or judge) — mirror that here instead of
        // expecting a model that deliberately wasn't built.
        if fast.members().is_empty() {
            return Vec::new();
        }
        ir::check(
            &self.global,
            &self.per_crate,
            fast,
            cx.semantic.expect("unused-deps requires the SemanticModel"),
        )
    }
}
