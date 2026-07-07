//! Unused-pub check: flags `pub` items that have no cross-crate references.
//! Items used only intra-crate get a "tighten to `pub(crate)`" suggestion;
//! items with no references at all get a "remove" suggestion (applied as a
//! whole-item deletion only under `--fix-auto-delete`, via the cascade).
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

use wl_diagnostic::Diagnostic;
use wl_lint_api::config::PerCrate;
use wl_lint_api::{LintContext, LintId, LintImpl, Requirements};

/// Number of unused-pub findings an internal crate must accumulate before we
/// emit the one-time `publish = true` hint. Used when the config leaves
/// `publish-hint-threshold` unset.
pub(super) const DEFAULT_PUBLISH_HINT_THRESHOLD: usize = 3;

pub mod cascade;
pub mod config;
mod ir;
#[cfg(test)]
mod tests;

pub use config::{KindFilter, UnusedPubConfig};

pub struct UnusedPub {
    config: PerCrate<UnusedPubConfig>,
}

impl UnusedPub {
    pub fn new(global: UnusedPubConfig, per_crate: HashMap<String, UnusedPubConfig>) -> Self {
        Self {
            config: PerCrate::new(global, per_crate),
        }
    }
}

impl LintImpl for UnusedPub {
    const ID: LintId = LintId::UnusedPub;
    // Semantic judgment; fast for manifests (publish resolution) and
    // workspace-relative paths.
    const REQUIRES: Requirements = Requirements {
        needs_fast: true,
        needs_semantic: true,
    };

    fn run(&self, cx: &LintContext<'_>) -> Vec<Diagnostic> {
        let Some((fast, semantic)) = cx.semantic_models(Self::ID) else {
            return Vec::new();
        };
        ir::check(&self.config, fast, semantic, cx.cfg_shadow)
    }
}
