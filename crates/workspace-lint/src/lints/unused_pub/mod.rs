//! Unused-pub check: flags `pub` items that have no cross-crate references.
//! Items used only intra-crate get a "tighten to `pub(crate)`" suggestion;
//! items with no references at all get a "remove" suggestion.
//!
//! The judged substrate is the rustc-extracted reference graph (the engine's
//! [`wl_engine::SemanticModel`]), which natively sees macro expansions,
//! `#[cfg]`-gated code (via the config matrix), associated items, and
//! trait-dispatch/FFI-export reachability — the classes of blind spot the
//! retired syn resolver documented as false positives/negatives.
//!
//! Semantics:
//!
//! - **Structural must-stay-`pub` guards.** A re-export target (E0364/E0365)
//!   and an item named in a more-visible signature (E0446 /
//!   `private_interfaces`) are exempt regardless of publish status —
//!   tightening them would not compile.
//! - **Publish-awareness.** A crate's library-public API is exempt as
//!   "external API surface" *only* when the crate declares it's published
//!   (`publish = true` or a registry list). A crate with `publish = false`
//!   or no `publish` field is treated as workspace-internal: its `pub` items
//!   go through the cross-crate check, so over-exposed internal APIs get
//!   flagged. `assume-all-public` opts out (treat every crate as external).
//!   When an internal crate accumulates `publish-hint-threshold` findings, a
//!   crate-level hint nudges `publish = true`.

use std::collections::HashMap;

use crate::config::GlobPattern;
use crate::lints::{Lint, LintContext, LintId, Requirements};
use wl_diagnostic::Diagnostic;

/// Number of unused-pub findings an internal crate must accumulate before we
/// emit the one-time `publish = true` hint. Used when the config leaves
/// `publish-hint-threshold` unset.
pub(super) const DEFAULT_PUBLISH_HINT_THRESHOLD: usize = 3;

pub(crate) mod cascade;
pub mod config;
mod ir;
mod surgery;
#[cfg(test)]
mod tests;

pub(crate) use config::{KindFilter, UnusedPubConfig};

pub(crate) struct UnusedPub {
    /// Workspace-wide params, used for any crate without a per-crate section.
    global: UnusedPubConfig,
    /// Per-crate params (keyed by Cargo package name), each *wholesale*
    /// replacing the global config for that crate. Empty for CLI single-check
    /// runs, which have no `[crates.*]` tier.
    per_crate: HashMap<String, UnusedPubConfig>,
}

impl UnusedPub {
    pub(crate) fn new(
        global: UnusedPubConfig,
        per_crate: HashMap<String, UnusedPubConfig>,
    ) -> Self {
        Self { global, per_crate }
    }

    pub(crate) fn from_cli(
        exclude_crates: Vec<String>,
        allowlist: Vec<String>,
        kinds: Vec<KindFilter>,
        exclude_paths: Vec<String>,
        suppress_intra_crate: bool,
    ) -> Self {
        Self::new(
            UnusedPubConfig {
                exclude_crates,
                allowlist: allowlist.iter().map(|p| GlobPattern::from_cli(p)).collect(),
                kinds,
                exclude_paths: exclude_paths
                    .iter()
                    .map(|p| GlobPattern::from_cli(p))
                    .collect(),
                suppress_intra_crate,
                // `--fix` deletion is opt-in via config only — there's no CLI
                // override because deletion is irreversible-without-git and we
                // want the choice to live in the project's config file (not a
                // forgotten shell history line).
                auto_delete: false,
                // Publish-awareness is config-only (no CLI flags): both live in the
                // project's config file. CLI single-lint runs keep the defaults.
                assume_all_public: false,
                publish_hint_threshold: None,
            },
            HashMap::new(),
        )
    }
}

impl Lint for UnusedPub {
    fn id(&self) -> LintId {
        LintId::UnusedPub
    }

    fn requirements(&self) -> Requirements {
        Requirements {
            needs_semantic: true,
            // Manifests (publish resolution), workspace-relative paths.
            needs_fast: true,
        }
    }

    fn check(&self, cx: &LintContext<'_>) -> Vec<Diagnostic> {
        let fast = cx.fast.expect("unused-pub requires the FastModel");
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
            cx.semantic.expect("unused-pub requires the SemanticModel"),
        )
    }
}
