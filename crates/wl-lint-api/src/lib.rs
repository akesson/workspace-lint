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

/// The declarative face of [`Lint`], for the common case where a lint's
/// identity and requirements are compile-time constants: implement this and
/// the blanket `impl<T: LintImpl> Lint for T` supplies the trait-object
/// surface the registry boxes. Only a lint whose id or requirements are
/// *runtime*-dependent would implement [`Lint`] directly (none do today).
pub trait LintImpl: 'static {
    /// The lint's [`LintId`] (see [`Lint::id`]).
    const ID: LintId;
    /// What shared models the runner must load (see [`Lint::requirements`]).
    /// Defaults to needing nothing.
    const REQUIRES: Requirements = Requirements {
        needs_fast: false,
        needs_semantic: false,
    };
    /// The check itself (see [`Lint::check`]).
    fn run(&self, cx: &LintContext<'_>) -> Vec<Diagnostic>;
}

impl<T: LintImpl> Lint for T {
    fn id(&self) -> LintId {
        T::ID
    }

    fn requirements(&self) -> Requirements {
        T::REQUIRES
    }

    fn check(&self, cx: &LintContext<'_>) -> Vec<Diagnostic> {
        self.run(cx)
    }
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
    /// The [`FastModel`] a `needs_fast` lint runs on. Panics (a wiring bug,
    /// not a user error) if the lint is run without it — the declared
    /// [`Requirements`] oblige the runner to load it first.
    pub fn fast_model(&self, id: LintId) -> &'a FastModel {
        self.fast
            .unwrap_or_else(|| panic!("{} requires the FastModel", id.short()))
    }

    /// Both models a semantic lint runs on, or `None` for a memberless
    /// workspace — the runner skips the semantic tier there (nothing to
    /// extract or judge), so lints must bail instead of expecting a model
    /// that deliberately wasn't built. Panics (a wiring bug, not a user
    /// error) if a lint that declared the requirements is run without the
    /// models on a workspace that *has* members.
    pub fn semantic_models(
        &self,
        id: LintId,
    ) -> Option<(&'a FastModel, &'a wl_engine::SemanticModel)> {
        let fast = self.fast_model(id);
        if fast.members().is_empty() {
            return None;
        }
        let semantic = self
            .semantic
            .unwrap_or_else(|| panic!("{} requires the SemanticModel", id.short()));
        Some((fast, semantic))
    }
}
