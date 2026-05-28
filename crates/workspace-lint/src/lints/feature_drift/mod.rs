//! Feature-flag drift between `Cargo.toml`'s `[features]` table and the
//! `#[cfg(feature = "...")]` references actually used in source.
//!
//! Two failure modes flagged:
//!
//! - **declared_never_gated**: a feature appears in `[features]` but is
//!   never referenced via `#[cfg(feature = "name")]` or
//!   `#[cfg_attr(feature = "name", ...)]` anywhere in the crate's source.
//!   `default` is excluded — cargo handles it specially.
//! - **gated_undeclared**: source contains `#[cfg(feature = "name")]` but
//!   `name` is not declared in `[features]`. Indicates a typo or a removed
//!   feature that left source references behind.
//!
//! v1 only scans outer attributes on items (the level the resolver visits).
//! Feature gates inside function bodies and macro bodies are tracked as
//! `known_false_negatives`.

use std::collections::BTreeSet;

use syn_workspace::Workspace;

use crate::diagnostic::Diagnostic;
use crate::diagnostic::builder::at_crate;
use crate::lints::{Lint, LintContext, LintId, Requirements};

pub struct FeatureDrift;

impl FeatureDrift {
    pub fn new() -> Self {
        Self
    }
}

impl Default for FeatureDrift {
    fn default() -> Self {
        Self::new()
    }
}

impl Lint for FeatureDrift {
    fn id(&self) -> LintId {
        LintId::FeatureDrift
    }

    fn requirements(&self) -> Requirements {
        Requirements {
            needs_workspace: true,
        }
    }

    fn check(&self, cx: &LintContext<'_>) -> Vec<Diagnostic> {
        let workspace = cx
            .workspace
            .expect("feature-drift lint requires Workspace (Requirements::needs_workspace)");
        check(workspace)
    }
}

pub fn check(workspace: &Workspace) -> Vec<Diagnostic> {
    let lint_id = LintId::FeatureDrift.id();
    let mut diagnostics = Vec::new();
    for krate in workspace.members() {
        let declared: BTreeSet<&str> = krate.declared_features.iter().map(String::as_str).collect();
        let used: BTreeSet<String> = krate
            .all_modules()
            .flat_map(|m| m.cfg_features.iter().cloned())
            .collect();
        let used_refs: BTreeSet<&str> = used.iter().map(String::as_str).collect();

        for &feat in &declared {
            if feat == "default" || feat.is_empty() {
                continue;
            }
            if used_refs.contains(feat) {
                continue;
            }
            let msg =
                format!("feature `{feat}` is declared in `[features]` but never gated in source");
            diagnostics.push(
                at_crate(lint_id, msg, krate.manifest_dir.clone())
                    .help(format!(
                        "either gate code with `#[cfg(feature = \"{feat}\")]` or remove `{feat}` from `[features]`",
                    ))
                    .note(format!("declared in `{}/Cargo.toml`", krate.name))
                    .build(),
            );
        }

        for feat in &used_refs {
            if declared.contains(feat) {
                continue;
            }
            let msg =
                format!("feature `{feat}` is gated in source but not declared in `[features]`");
            diagnostics.push(
                at_crate(lint_id, msg, krate.manifest_dir.clone())
                    .help(format!(
                        "add `{feat} = []` to the `[features]` table of `{}/Cargo.toml`, or remove the `cfg(feature = \"{feat}\")` references",
                        krate.name,
                    ))
                    .build(),
            );
        }
    }
    diagnostics
}
