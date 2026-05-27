//! Workspace discovery — turn a directory into a list of [`Crate`] entries.
//!
//! Uses `cargo_metadata` to invoke `cargo metadata` against the workspace
//! root, then materializes each workspace package as a [`Crate`] with a
//! placeholder empty [`Module`] root. Subsequent resolver tiers (Tier 2 in
//! `resolve::module_tree`) populate the module trees by walking each crate's
//! source directory.
//!
//! External crates (transitive cargo deps) are not yet materialized — the
//! list is just workspace members. The [`Workspace`] model carries
//! `is_workspace_member` so external crates can be added later without an
//! API change.

use std::path::Path;

use cargo_metadata::MetadataCommand;

use crate::resolve::{Crate, Error, Result, module_tree};

/// Run `cargo metadata` on the workspace at `root` and return one [`Crate`]
/// per workspace member.
pub fn load_members(root: &Path) -> Result<Vec<Crate>> {
    let manifest = root.join("Cargo.toml");
    let metadata = MetadataCommand::new()
        .manifest_path(&manifest)
        .exec()
        .map_err(|e| {
            Error::Manifest(format!(
                "cargo metadata failed for {}: {e}",
                manifest.display()
            ))
        })?;

    let mut out = Vec::new();
    for pkg in metadata.workspace_packages() {
        let manifest_dir = pkg
            .manifest_path
            .parent()
            .map(|p| p.as_std_path().to_path_buf())
            .ok_or_else(|| {
                Error::Manifest(format!(
                    "manifest path has no parent: {}",
                    pkg.manifest_path
                ))
            })?;

        // Cargo crate names use hyphens (e.g. `data-models`), but source code
        // references them with underscores (`use data_models::...`). The
        // resolver stores canonical paths in code form so they line up with
        // what bindings see; `Crate.name` keeps the cargo form so user-facing
        // diagnostics and `from`-pattern matching see the same string users
        // wrote in `Cargo.toml`.
        let code_name = pkg.name.replace('-', "_");
        let root_module = module_tree::build_crate_tree(&manifest_dir, &code_name)?;

        out.push(Crate {
            name: pkg.name.to_string(),
            version: pkg.version.to_string(),
            manifest_dir,
            is_workspace_member: true,
            root: root_module,
        });
    }

    Ok(out)
}
