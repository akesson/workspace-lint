//! The lean workspace loader behind [`FastModel`]: one `cargo metadata
//! --no-deps` call plus a parsed [`Manifest`] per member. No source file is
//! read or parsed — this tier is deliberately just metadata + manifests.

use std::path::{Path, PathBuf};

use cargo_metadata::MetadataCommand;

use super::{Manifest, Result};

/// The build-free workspace model: root, members, and their parsed manifests.
pub struct FastModel {
    /// `cargo metadata`'s `workspace_root` (absolute) — the same base the
    /// member `manifest_dir`s carry, so [`FastModel::crate_relative_path`]'s
    /// plain `strip_prefix` agrees by construction.
    root: PathBuf,
    root_manifest: Manifest,
    /// Workspace members, sorted by name for deterministic iteration.
    members: Vec<CrateInfo>,
}

/// One workspace member: identity, location, and its parsed `Cargo.toml`.
pub struct CrateInfo {
    /// Cargo package name (hyphens preserved).
    pub name: String,
    /// Absolute directory containing the member's `Cargo.toml`.
    pub manifest_dir: PathBuf,
    manifest: Manifest,
}

impl FastModel {
    /// Load the workspace whose root `Cargo.toml` lives in `root`.
    ///
    /// `--no-deps`: only workspace members are materialized, so cargo's
    /// dependency *resolution* is pure overhead — and it would force a
    /// resolvable graph (network / a populated registry / a lockfile) just to
    /// load. Skipping it keeps the load offline and sub-second.
    pub fn load(root: &Path) -> Result<Self> {
        let metadata = MetadataCommand::new()
            .manifest_path(root.join("Cargo.toml"))
            .no_deps()
            .exec()?;
        // Store the metadata's `workspace_root`, not the caller's argument:
        // the member `manifest_dir`s below are absolute paths from the same
        // metadata call, so relative-path queries agree without leaning on
        // the canonicalization fallback (a caller root of `.` would).
        let root = metadata.workspace_root.as_std_path().to_path_buf();
        let root_manifest = Manifest::load(root.join("Cargo.toml"))?;
        let mut members = metadata
            .workspace_packages()
            .into_iter()
            .map(|pkg| {
                let manifest_path = pkg.manifest_path.as_std_path();
                let manifest_dir = manifest_path
                    .parent()
                    .expect("a Cargo.toml path always has a parent directory")
                    .to_path_buf();
                Ok(CrateInfo {
                    name: pkg.name.to_string(),
                    manifest_dir,
                    manifest: Manifest::load(manifest_path)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        members.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Self {
            root,
            root_manifest,
            members,
        })
    }

    /// Workspace root directory (absolute, from `cargo metadata`).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Parsed root `Cargo.toml`. Carries the `[workspace.dependencies]`
    /// table (queried by centralized-dep analyses) and the raw source
    /// bytes (useful for comment-based directive scanners).
    pub fn root_manifest(&self) -> &Manifest {
        &self.root_manifest
    }

    /// The workspace member crates, sorted by name.
    pub fn members(&self) -> &[CrateInfo] {
        &self.members
    }

    /// Look up a workspace member by its Cargo-form name (the value users
    /// write in `Cargo.toml`, hyphens preserved).
    pub fn member_by_name(&self, name: &str) -> Option<&CrateInfo> {
        self.members.iter().find(|c| c.name == name)
    }

    /// Strip the workspace root prefix from `path` and return a path
    /// relative to [`Self::root`]. Falls back to a clone of `path` when
    /// the input doesn't start with the workspace root — keeps callers
    /// (mostly diagnostic-builder lints) one-liners regardless of
    /// whether the input was inside or outside the workspace tree.
    ///
    /// Use this for any anchor or rendered path that's expected to round-
    /// trip with a `# workspace-lint: …` suppression directive: the
    /// directive scanner emits anchors against workspace-relative paths,
    /// so any absolute `cargo_metadata`-derived path needs to come back
    /// through here before being anchored.
    pub fn crate_relative_path(&self, path: &Path) -> PathBuf {
        if let Ok(rel) = path.strip_prefix(&self.root) {
            return rel.to_path_buf();
        }
        // Two follow-up attempts handle the platform asymmetries that bite
        // in CI:
        //   - macOS: `/var` ↔ `/private/var` symlink dance — only one side
        //     canonicalizes.
        //   - Windows: `Path::canonicalize` returns a `\\?\` UNC prefix that
        //     the cargo_metadata-derived `manifest_dir` doesn't carry,
        //     so canonicalising only the root still leaves a mismatch.
        // Canonicalising both sides at once normalises away both.
        if let Ok(abs_root) = self.root.canonicalize() {
            if let Ok(rel) = path.strip_prefix(&abs_root) {
                return rel.to_path_buf();
            }
            if let Ok(abs_path) = path.canonicalize()
                && let Ok(rel) = abs_path.strip_prefix(&abs_root)
            {
                return rel.to_path_buf();
            }
        }
        path.to_path_buf()
    }
}

impl CrateInfo {
    /// In-code form of the crate name (Cargo hyphens replaced with `_`).
    ///
    /// [`CrateInfo::name`] is the Cargo form (`data-models`), but source code
    /// references the crate as `data_models::…` — prefer this method over
    /// hand-rolling `name.replace('-', "_")`.
    pub fn code_name(&self) -> String {
        self.name.replace('-', "_")
    }

    /// Parsed `Cargo.toml` for this crate. Use this in preference to
    /// re-parsing the file from disk.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This repository's own workspace — the one real cargo workspace every
    /// test run has available.
    fn load_this_workspace() -> FastModel {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        FastModel::load(&root).expect("this repo is a loadable workspace")
    }

    #[test]
    fn members_are_the_workspace_crates_sorted_by_name() {
        let model = load_this_workspace();
        let names: Vec<&str> = model.members().iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "syn-workspace",
                "syn-workspace-marker",
                "wl-engine",
                "wl-ir",
                "workspace-lint",
                "workspace-lint-marker",
            ]
        );
    }

    #[test]
    fn root_manifest_carries_workspace_dependencies() {
        let model = load_this_workspace();
        assert!(
            !model.root_manifest().workspace_dep_names().is_empty(),
            "the repo root declares [workspace.dependencies]"
        );
    }

    #[test]
    fn crate_relative_path_round_trips_a_member_manifest_dir() {
        let model = load_this_workspace();
        let member = model.member_by_name("wl-engine").unwrap();
        assert_eq!(
            model.crate_relative_path(&member.manifest_dir),
            Path::new("crates/wl-engine")
        );
    }

    #[test]
    fn member_by_name_is_cargo_form_and_code_name_normalizes() {
        let model = load_this_workspace();
        let member = model.member_by_name("wl-ir").expect("wl-ir is a member");
        assert_eq!(member.code_name(), "wl_ir");
    }
}
