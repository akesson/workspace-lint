//! The `Lint` trait vocabulary, per-lint modules, and the config primitives
//! and helpers shared across them.
//!
//! Every check lives in `crates/wl-lints/src/<name>/` and implements [`Lint`].
//! The shared [`LintContext`] carries the optional build-free [`FastModel`] and
//! the optional rustc-extracted [`wl_engine::SemanticModel`]; everything else a
//! lint needs (its configuration, glob matchers, etc.) is captured inside the
//! lint instance at construction time. The *registry* that binds enabled lints
//! to a loaded `Config` is the binary's composition root
//! (`crates/workspace-lint/src/registry.rs`), not part of this crate.
//!
//! ## Adding a new lint
//!
//! 1. Create `crates/wl-lints/src/<name>/{mod.rs,config.rs,tests.rs}`.
//! 2. Add a [`LintId`] variant in `lints_id.rs`.
//! 3. Add a line in the binary's `registry::registry` wiring the lint up to its
//!    config block.
//! 4. Add a scenario in the binary's `messages::scenarios()` (asserted by the
//!    registry-coverage test).
//! 5. Add fixtures under `tests/cases/<name>/` (and `tests/fixtures/fix__<name>/`
//!    if the lint emits machine-applicable structural fixes).

pub mod lints_id;

pub mod architecture;
pub mod centralized_deps;
pub mod cli_crate_version;
pub mod crate_size;
pub mod duplicate_code;
pub mod feature_drift;
pub mod file_size;
pub mod freshness;
pub mod module_tree;
pub mod stale_git_index;
pub mod unused_deps;
pub mod unused_pub;

/// Strongly-typed config primitives (lint levels, glob patterns) shared by the
/// per-lint config structs and by the binary's `Config` aggregator.
pub mod config;
/// The `GIT_*`-scrubbing chokepoint for spawning git (see [`git::command`]).
pub mod git;
/// Small cross-cutting helpers shared across lints and the binary.
pub mod util;

/// Production-only line counting shared by `crate-size` and `file-size`.
mod shipped_source;

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
    pub(crate) fn semantic_models(
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
