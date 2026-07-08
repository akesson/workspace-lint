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

use wl_diagnostic::Diagnostic;
use wl_lint_api::config::PerCrate;
use wl_lint_api::{LintContext, LintId, LintImpl, Requirements};

pub mod config;
mod ir;
#[cfg(test)]
mod tests;

pub use config::UnusedDepsConfig;

pub struct UnusedDeps {
    config: PerCrate<UnusedDepsConfig>,
}

impl UnusedDeps {
    pub fn new(global: UnusedDepsConfig, per_crate: HashMap<String, UnusedDepsConfig>) -> Self {
        Self {
            config: PerCrate::new(global, per_crate),
        }
    }

    pub fn from_cli(ignore: Vec<String>) -> Self {
        Self::new(UnusedDepsConfig { ignore }, HashMap::new())
    }
}

impl LintImpl for UnusedDeps {
    const ID: LintId = LintId::UnusedDeps;
    const DOC: &'static str = include_str!("DOC.md");
    // Semantic judgment; fast for manifests, doc-fence refs, and
    // workspace-relative paths.
    const REQUIRES: Requirements = Requirements {
        needs_fast: true,
        needs_semantic: true,
    };

    fn run(&self, cx: &LintContext<'_>) -> Vec<Diagnostic> {
        let Some((fast, semantic)) = cx.semantic_models(Self::ID) else {
            return Vec::new();
        };
        ir::check(&self.config, fast, semantic)
    }
}
