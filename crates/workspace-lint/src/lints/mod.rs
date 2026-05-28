//! The `Lint` trait, runtime registry, and per-lint modules.
//!
//! Every check lives in `crates/workspace-lint/src/lints/<name>/` and
//! implements [`Lint`]. The shared [`LintContext`] carries the optional
//! resolver-loaded [`Workspace`]; everything else a lint needs (its
//! configuration, glob matchers, etc.) is captured inside the lint
//! instance at construction time.
//!
//! ## Adding a new lint
//!
//! 1. Create `lints/<name>/{mod.rs,config.rs,tests.rs}`.
//! 2. Add a [`LintId`] variant in `lints_id.rs`.
//! 3. Add a line in [`registry`] wiring the lint up to its config block.
//! 4. Add a scenario in `messages::scenarios()` (asserted by the registry
//!    coverage test in [`lints_id::tests`]).
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
pub mod visibility;

pub use lints_id::LintId;

use crate::config::Config;
use crate::diagnostic::Diagnostic;
use syn_workspace::Workspace;

/// Object-safe trait every check implements. Lints are owned `Box<dyn Lint>`
/// instances built once at startup from the user's [`Config`]; the
/// [`LintContext`] passed to [`Lint::check`] carries only state shared across
/// lints (currently just the resolver-loaded [`Workspace`]).
pub trait Lint: 'static {
    /// Stable identity used by the suppression map, the `[lints]` severity
    /// table, and the snapshot-coverage tests.
    fn id(&self) -> LintId;

    /// Declared up-front so the runner can decide whether to pay the
    /// `syn_workspace::Workspace::load` cost.
    fn requirements(&self) -> Requirements {
        Requirements::default()
    }

    /// Produce the lint's diagnostics. Lints that don't need a `Workspace`
    /// can ignore [`LintContext::workspace`]; lints that do need one set
    /// `Requirements::needs_workspace = true` so the runner loads it before
    /// calling `check`.
    fn check(&self, cx: &LintContext<'_>) -> Vec<Diagnostic>;
}

/// Static description of what shared resources a lint needs. Inspected
/// before any check runs so the runner can skip expensive setup (notably
/// `Workspace::load`) when no enabled lint requires it.
#[derive(Default, Clone, Copy, Debug)]
pub struct Requirements {
    pub needs_workspace: bool,
}

/// Shared, per-run inputs passed to every [`Lint::check`] call.
pub struct LintContext<'a> {
    pub workspace: Option<&'a Workspace>,
}

/// Build the runtime registry of enabled lints from the user's configuration.
///
/// Adding a lint is one new line here plus one folder under `lints/`.
pub fn registry(config: &Config) -> Vec<Box<dyn Lint>> {
    let mut out: Vec<Box<dyn Lint>> = Vec::new();

    if let Some(ref ac) = config.architecture
        && !ac.rules.is_empty()
    {
        out.push(Box::new(architecture::Architecture::new(ac.clone())));
    }
    if config.checks.centralized_deps {
        out.push(Box::new(centralized_deps::CentralizedDeps::new()));
    }
    if let Some(ref cv) = config.cli_crate_version {
        out.push(Box::new(cli_crate_version::CliCrateVersion::new(
            cv.clone(),
        )));
    }
    if let Some(ref cs) = config.crate_size {
        out.push(Box::new(crate_size::CrateSize::new(cs.clone())));
    }
    if config.checks.feature_drift {
        out.push(Box::new(feature_drift::FeatureDrift::new()));
    }
    if let Some(ref fs) = config.file_size {
        out.push(Box::new(file_size::FileSize::new(fs.clone())));
    }
    if let Some(ref fr) = config.freshness {
        out.push(Box::new(freshness::Freshness::new(fr.clone())));
    }
    if config.checks.module_tree {
        out.push(Box::new(module_tree::ModuleTree::new()));
    }
    // stale-git-index is always-on (no config gate).
    out.push(Box::new(stale_git_index::StaleGitIndex::new()));
    if let Some(ref ud) = config.unused_deps {
        out.push(Box::new(unused_deps::UnusedDeps::new(ud.clone())));
    }
    if let Some(ref up) = config.unused_pub {
        out.push(Box::new(unused_pub::UnusedPub::new(up.clone())));
    }
    if config.checks.visibility {
        out.push(Box::new(visibility::Visibility::new()));
    }

    out
}
