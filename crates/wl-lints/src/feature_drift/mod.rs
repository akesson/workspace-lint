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
//! v1 only scans outer attributes on items (the level the walker visits).
//! Feature gates inside function bodies and macro bodies are tracked as
//! `known_false_negatives`.

use std::collections::BTreeSet;

use wl_engine::fast::FastModel;

use crate::{Lint, LintContext, LintId, Requirements};
use wl_diagnostic::Diagnostic;
use wl_diagnostic::builder::at_crate;

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
            needs_fast: true,
            ..Requirements::default()
        }
    }

    fn check(&self, cx: &LintContext<'_>) -> Vec<Diagnostic> {
        let fast = cx
            .fast
            .expect("feature-drift lint requires FastModel (Requirements::needs_fast)");
        check(fast)
    }
}

pub(crate) fn check(fast: &FastModel) -> Vec<Diagnostic> {
    let lint_id = LintId::FeatureDrift.id();
    let mut diagnostics = Vec::new();
    for krate in fast.members() {
        let declared: BTreeSet<&str> = krate
            .declared_features()
            .iter()
            .map(String::as_str)
            .collect();
        let used: BTreeSet<String> = krate
            .all_modules()
            .flat_map(|m| m.cfg_features.iter().cloned())
            .collect();
        let used_refs: BTreeSet<&str> = used.iter().map(String::as_str).collect();

        for &feat in &declared {
            if !should_flag_declared(
                feat,
                krate.feature_values().get(feat).map(Vec::as_slice),
                &used_refs,
            ) {
                continue;
            }
            let msg =
                format!("feature `{feat}` is declared in `[features]` but never gated in source");
            diagnostics.push(
                at_crate(lint_id, msg, fast.crate_relative_path(&krate.manifest_dir))
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
                at_crate(lint_id, msg, fast.crate_relative_path(&krate.manifest_dir))
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

/// Pure decision for the **declared_never_gated** branch: should `feat` be
/// flagged as declared in `[features]` but never gated in source?
///
/// - `default` / the empty string are excluded (cargo handles `default`
///   specially; the empty string is never a real feature name).
/// - Only "leaf" features (empty activation list) gate code directly. A
///   feature whose list is non-empty forwards to a dependency (`dep:foo`,
///   `foo/bar`, `foo?/bar`) or to another feature (umbrella) — and cargo
///   synthesizes such a list for every implicit optional-dependency feature.
///   Those legitimately never appear in a `#[cfg(feature = "...")]` gate, so
///   flagging them is a false positive. `activation` is the feature's value
///   list from `Cargo.toml`'s `[features]` table (`None` if the feature isn't
///   present in the map at all).
/// - A leaf feature that *is* referenced by a `cfg(feature = ...)` gate is
///   fine.
///
/// See `feature_drift/true_negatives/{feature_gates_optional_dep,
/// implicit_optional_dep_feature, umbrella_feature, weak_dep_activation}`.
fn should_flag_declared(
    feat: &str,
    activation: Option<&[String]>,
    used_refs: &BTreeSet<&str>,
) -> bool {
    if feat == "default" || feat.is_empty() {
        return false;
    }
    if activation.is_some_and(|vals| !vals.is_empty()) {
        return false;
    }
    !used_refs.contains(feat)
}

#[cfg(test)]
mod tests;
