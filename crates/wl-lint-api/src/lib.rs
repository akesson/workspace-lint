//! The lint API: everything a lint implementation builds on that isn't
//! judgment.
//!
//! - The [`Lint`] trait vocabulary ([`Requirements`], [`LintContext`]) and
//!   [`LintId`], the canonical identity of every lint.
//! - The strongly-typed config primitives ([`config`]: `LintLevel`,
//!   `GlobPattern`, …) the per-lint config structs are built from.
//! - The shared [`git`] (the `GIT_*`-scrub chokepoint) and [`util`] helpers.
//! - [`surgery`]: the byte-exact source-editing machinery behind the
//!   structural fixes (`--fix-auto-delete` whole-item deletion, dangling
//!   `use` excision).
//!
//! The lint *implementations* live in `wl-lints` (one module per lint); the
//! registry that binds enabled lints to a loaded `Config` is the binary's
//! composition root (`crates/workspace-lint/src/registry.rs`). Layering:
//! `workspace-lint` → `wl-lints` → `wl-lint-api` → {`wl-diagnostic`,
//! `wl-engine`}.

pub mod config;
pub mod git;
pub mod lints_id;
pub mod surgery;
pub mod util;

pub use lints_id::LintId;

use wl_diagnostic::Diagnostic;
use wl_engine::fast::FastModel;

/// Object-safe trait every check implements. Lints are owned `Box<dyn Lint>`
/// instances built once at startup from the user's `Config` (by the binary's
/// `registry`); the [`LintContext`] passed to [`Lint::check`] carries only
/// state shared across lints (the build-free [`FastModel`] and the
/// rustc-extracted [`wl_engine::SemanticModel`]).
pub trait Lint: 'static {
    /// Stable identity, asserted against `LintId::ALL` by the registry-coverage
    /// and CLI-dispatch tests, and used by the runner to build the "ran" set
    /// for `stale-expect`.
    fn id(&self) -> LintId;

    /// Declared up-front so the runner can decide whether to pay the
    /// `FastModel::load` / extraction cost.
    fn requirements(&self) -> Requirements {
        Requirements::default()
    }

    /// Produce the lint's diagnostics. Lints that don't need a shared model
    /// can ignore [`LintContext::fast`] / [`LintContext::semantic`]; lints
    /// that do need one set the matching [`Requirements`] flag so the runner
    /// loads it before calling `check`.
    fn check(&self, cx: &LintContext<'_>) -> Vec<Diagnostic>;
}

/// Static description of what shared resources a lint needs. Inspected
/// before any check runs so the runner can skip expensive setup (notably
/// the rustc-backed extraction) when no enabled lint requires it.
#[derive(Default, Clone, Copy, Debug)]
pub struct Requirements {
    /// The build-free [`FastModel`] (`cargo metadata` + manifests only).
    pub needs_fast: bool,
    /// The rustc-extracted [`wl_engine::SemanticModel`] (runs the full tier:
    /// embedded dylint extraction on the pinned toolchain + Phase-2 assembly).
    /// Skipped under `--fast-only`.
    pub needs_semantic: bool,
}

/// Shared, per-run inputs passed to every [`Lint::check`] call.
pub struct LintContext<'a> {
    pub fast: Option<&'a FastModel>,
    pub semantic: Option<&'a wl_engine::SemanticModel>,
    /// The cfg-shadow index (regions no `[engine]` config compiles), built by
    /// the runner alongside the semantic tier. `unused-pub` uses it to mark
    /// `Unused` findings that are *possibly used* under an uncovered cfg
    /// (the report-time twin of the `--fix-auto-delete` veto).
    pub cfg_shadow: Option<&'a wl_engine::coverage::CfgShadow>,
}

impl<'a> LintContext<'a> {
    /// Both models a semantic lint runs on, or `None` for a memberless
    /// workspace — the runner skips the semantic tier there (nothing to
    /// extract or judge), so lints must bail instead of expecting a model
    /// that deliberately wasn't built. Panics (a wiring bug, not a user
    /// error) if a lint that declared the requirements is run without the
    /// models on a workspace that *has* members.
    pub fn semantic_models(
        &self,
        lint: &str,
    ) -> Option<(&'a FastModel, &'a wl_engine::SemanticModel)> {
        let fast = self
            .fast
            .unwrap_or_else(|| panic!("{lint} requires the FastModel"));
        if fast.members().is_empty() {
            return None;
        }
        let semantic = self
            .semantic
            .unwrap_or_else(|| panic!("{lint} requires the SemanticModel"));
        Some((fast, semantic))
    }
}
