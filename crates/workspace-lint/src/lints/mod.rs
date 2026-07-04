//! The `Lint` trait, runtime registry, and per-lint modules.
//!
//! Every check lives in `crates/workspace-lint/src/lints/<name>/` and
//! implements [`Lint`]. The shared [`LintContext`] carries the optional
//! build-free [`FastModel`] and the optional rustc-extracted
//! [`wl_engine::SemanticModel`]; everything else a lint needs (its
//! configuration, glob matchers, etc.) is captured inside the lint instance
//! at construction time.
//!
//! ## Adding a new lint
//!
//! 1. Create `lints/<name>/{mod.rs,config.rs,tests.rs}`.
//! 2. Add a [`LintId`] variant in `lints_id.rs`.
//! 3. Add a line in [`registry`] wiring the lint up to its config block.
//! 4. Add a scenario in `messages::scenarios()` (asserted by the registry
//!    coverage test in `lints_id::tests`).
//! 5. Add fixtures under `tests/cases/<name>/` (and `tests/fixtures/fix__<name>/`
//!    if the lint emits machine-applicable structural fixes).

pub mod lints_id;

pub mod architecture;
pub mod centralized_deps;
pub mod cli_crate_version;
pub mod crate_size;
pub mod feature_drift;
pub mod file_size;
pub mod freshness;
pub mod module_tree;
pub mod stale_git_index;
pub mod unused_deps;
pub mod unused_pub;

/// Production-only line counting shared by `crate-size` and `file-size`.
mod shipped_source;

pub(crate) use lints_id::LintId;

use crate::config::{Config, LintLevel};
use wl_diagnostic::Diagnostic;
use wl_engine::fast::FastModel;

/// Object-safe trait every check implements. Lints are owned `Box<dyn Lint>`
/// instances built once at startup from the user's [`Config`]; the
/// [`LintContext`] passed to [`Lint::check`] carries only state shared across
/// lints (the build-free [`FastModel`] and the rustc-extracted
/// [`wl_engine::SemanticModel`]).
pub(crate) trait Lint: 'static {
    /// Stable identity, asserted against `LintId::ALL` by the registry-coverage
    /// and CLI-dispatch tests. The runtime routes on each diagnostic's string
    /// `lint` field, so this is test-only today.
    #[allow(dead_code)]
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
pub(crate) struct Requirements {
    /// The build-free [`FastModel`] (`cargo metadata` + manifests only).
    pub needs_fast: bool,
    /// The rustc-extracted [`wl_engine::SemanticModel`] (runs the full tier:
    /// embedded dylint extraction on the pinned toolchain + Phase-2 assembly).
    /// Skipped under `--fast-only`.
    pub needs_semantic: bool,
}

/// Shared, per-run inputs passed to every [`Lint::check`] call.
pub(crate) struct LintContext<'a> {
    pub fast: Option<&'a FastModel>,
    pub semantic: Option<&'a wl_engine::SemanticModel>,
}

/// `true` when `id` is enabled *anywhere* — its global effective level isn't
/// `allow`, **or** some per-crate block turns it back on. A lint runs once for
/// the whole workspace and `apply_lint_levels` later drops the diagnostics that
/// land in crates where the effective level is `allow`, so "on for one crate"
/// means the lint must run. This is half the enable rule; *policy* lints
/// additionally require their config table to be present (checked inline below).
fn level_on(config: &Config, id: LintId) -> bool {
    if config.lints.effective(id) != LintLevel::Allow {
        return true;
    }
    config
        .crates
        .keys()
        .any(|name| config.effective_level(id, Some(name)) != LintLevel::Allow)
}

/// Build the runtime registry of enabled lints from the user's configuration.
///
/// Enablement is uniform: a lint runs iff its effective level isn't `allow`
/// **and** — for [`LintId::requires_config`] (policy) lints — its config
/// table is present. Structural lints need no table, so they're on by default.
/// Adding a lint is one new block here plus one folder under `lints/`.
pub(crate) fn registry(config: &Config) -> Vec<Box<dyn Lint>> {
    let mut out: Vec<Box<dyn Lint>> = Vec::new();

    // --- policy lints: gated on `level != allow` AND a present config table ---
    if level_on(config, LintId::Architecture)
        && let Some(ref ac) = config.architecture
        && ac.is_active()
    {
        out.push(Box::new(architecture::Architecture::new(ac.clone())));
    }
    if level_on(config, LintId::CliCrateVersion)
        && let Some(ref cv) = config.cli_crate_version
    {
        out.push(Box::new(cli_crate_version::CliCrateVersion::new(
            cv.clone(),
        )));
    }
    if level_on(config, LintId::CrateSize)
        && let Some(ref cs) = config.crate_size
    {
        out.push(Box::new(crate_size::CrateSize::new(cs.clone())));
    }
    if level_on(config, LintId::FileSize)
        && let Some(ref fs) = config.file_size
    {
        out.push(Box::new(file_size::FileSize::new(fs.clone())));
    }
    if level_on(config, LintId::Freshness)
        && let Some(ref fr) = config.freshness
    {
        out.push(Box::new(freshness::Freshness::new(fr.clone())));
    }

    // --- structural lints: on by default (no required config) ---
    if level_on(config, LintId::CentralizedDeps) {
        out.push(Box::new(centralized_deps::CentralizedDeps::new()));
    }
    if level_on(config, LintId::FeatureDrift) {
        out.push(Box::new(feature_drift::FeatureDrift::new()));
    }
    if level_on(config, LintId::ModuleTree) {
        out.push(Box::new(module_tree::ModuleTree::new()));
    }
    if level_on(config, LintId::StaleGitIndex) {
        out.push(Box::new(stale_git_index::StaleGitIndex::new()));
    }
    if level_on(config, LintId::UnusedDeps) {
        out.push(Box::new(unused_deps::UnusedDeps::new(
            config.unused_deps.clone().unwrap_or_default(),
            config.unused_deps_overrides(),
        )));
    }
    if level_on(config, LintId::UnusedPub) {
        out.push(Box::new(unused_pub::UnusedPub::new(
            config.unused_pub.clone().unwrap_or_default(),
            config.unused_pub_overrides(),
        )));
    }

    out
}
