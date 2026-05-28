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
//! v1 only scans outer attributes on items (the level the resolver
//! visits). Feature gates inside function bodies (`if cfg!(feature = ...)`)
//! and inside macro bodies are tracked as `known_false_negatives`.

use std::collections::BTreeSet;

use syn_workspace::{Module, Workspace};

use crate::diagnostic::Diagnostic;
use crate::diagnostic::builder::at_crate;

pub const LINT: &str = crate::lints::LintId::FeatureDrift.id();

pub fn check(workspace: &Workspace) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for krate in workspace.crates() {
        if !krate.is_workspace_member {
            continue;
        }
        let declared: BTreeSet<&str> = krate.declared_features.iter().map(String::as_str).collect();
        let mut used: BTreeSet<String> = BTreeSet::new();
        // Feature gates can appear in any target (build.rs gates
        // build-time codegen, tests gate integration paths). Walk all
        // targets so we don't miss a `#[cfg(feature = "x")]` outside lib.
        for target in &krate.targets {
            collect_cfg_features(&target.root, &mut used);
        }
        let used_refs: BTreeSet<&str> = used.iter().map(String::as_str).collect();

        // declared_never_gated — skip `default` and any feature literally
        // named "" (defensive; cargo would reject empty names anyway).
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
                at_crate(LINT, msg, krate.manifest_dir.clone())
                    .help(format!(
                        "either gate code with `#[cfg(feature = \"{feat}\")]` or remove `{feat}` from `[features]`",
                    ))
                    .note(format!("declared in `{}/Cargo.toml`", krate.name))
                    .build(),
            );
        }

        // gated_undeclared — for each referenced feature not in the declared
        // set. Skip features that the crate activates transitively via deps
        // (those have a `/` separator in cargo syntax, but a `#[cfg]` value
        // is just a bare feature name).
        for feat in &used_refs {
            if declared.contains(feat) {
                continue;
            }
            let msg =
                format!("feature `{feat}` is gated in source but not declared in `[features]`");
            diagnostics.push(
                at_crate(LINT, msg, krate.manifest_dir.clone())
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

fn collect_cfg_features(module: &Module, out: &mut BTreeSet<String>) {
    for f in &module.cfg_features {
        out.insert(f.clone());
    }
    for sub in &module.submodules {
        collect_cfg_features(sub, out);
    }
}
