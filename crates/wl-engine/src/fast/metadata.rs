//! The lean workspace loader behind [`FastModel`]: one `cargo metadata
//! --no-deps` call, a parsed [`Manifest`] per member, and each member's lean
//! syntactic module trees (built by the sibling `module_tree` walker). No
//! compilation, no name resolution — parsing member sources is the ceiling of
//! this tier.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cargo_metadata::MetadataCommand;

use super::types::{Module, Target};
use super::{FastError, Manifest, Result, module_tree};

/// The build-free workspace model: root, members, and their parsed manifests
/// plus lean syntactic module trees.
pub struct FastModel {
    /// `cargo metadata`'s `workspace_root` (absolute) — the same base the
    /// member `manifest_dir`s carry, so [`FastModel::crate_relative_path`]'s
    /// plain `strip_prefix` agrees by construction.
    root: PathBuf,
    /// `cargo metadata`'s `target_directory` (absolute). Everything under it
    /// is build-generated — never an author-editable lint surface.
    target_directory: PathBuf,
    root_manifest: Manifest,
    /// Workspace members, sorted by name for deterministic iteration.
    members: Vec<CrateInfo>,
}

/// One workspace member: identity, location, its parsed `Cargo.toml`, its
/// `[features]` tables (from `cargo metadata`), and its syntactic module
/// trees.
pub struct CrateInfo {
    /// Cargo package name (hyphens preserved).
    pub name: String,
    /// Absolute directory containing the member's `Cargo.toml`.
    pub manifest_dir: PathBuf,
    manifest: Manifest,
    declared_features: Vec<String>,
    feature_values: BTreeMap<String, Vec<String>>,
    targets: Vec<Target>,
    orphan_files: Vec<PathBuf>,
    generated_files: Vec<PathBuf>,
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
        let target_directory = metadata.target_directory.as_std_path().to_path_buf();
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
                let mut declared_features: Vec<String> = pkg.features.keys().cloned().collect();
                declared_features.sort();
                // Full activation lists (cargo synthesizes `foo = ["dep:foo"]`
                // for an implicit optional-dependency feature) so consumers can
                // tell a code-gating "leaf" feature (empty list) from a
                // dependency/feature "plumbing" one.
                let feature_values: BTreeMap<String, Vec<String>> = pkg
                    .features
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                let targets = module_tree::build_targets(pkg, &manifest_dir)?;
                let orphan_files = module_tree::compute_orphans(&manifest_dir, &targets);
                // Union of every target's spliced `include!` files (each module
                // records its own in [`Module::generated_files`]).
                let generated_files: Vec<PathBuf> = targets
                    .iter()
                    .flat_map(|t| t.all_modules())
                    .flat_map(|m| m.generated_files.iter().cloned())
                    .collect();
                Ok(CrateInfo {
                    name: pkg.name.to_string(),
                    manifest_dir,
                    manifest: Manifest::load(manifest_path)?,
                    declared_features,
                    feature_values,
                    targets,
                    orphan_files,
                    generated_files,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        members.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Self {
            root,
            target_directory,
            root_manifest,
            members,
        })
    }

    /// Workspace root directory (absolute, from `cargo metadata`).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Cargo's target directory (absolute, from `cargo metadata`). A source
    /// file under it — `OUT_DIR` content spliced via `include!`, dylint's
    /// nested target — is build-generated, so lints must not anchor findings
    /// (or propose fixes) there.
    pub fn target_directory(&self) -> &Path {
        &self.target_directory
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

    /// `krate`'s `publish` field with `publish.workspace = true` resolved
    /// against the root manifest's `[workspace.package] publish`. (A root
    /// that itself inherits is nonsensical; treated as absent.) Mirrors the
    /// syn resolver's `resolved_publish` — the primitive behind the
    /// publish-aware external-API exemption in `unused-pub`.
    pub fn resolved_publish(&self, krate: &CrateInfo) -> super::manifest::Publish {
        use super::manifest::Publish;
        match krate.manifest().publish() {
            Publish::Inherited => match self.root_manifest.workspace_package_publish() {
                Publish::Inherited => Publish::Absent,
                resolved => resolved,
            },
            other => other,
        }
    }

    /// Look up a workspace member by its Cargo-form name (the value users
    /// write in `Cargo.toml`, hyphens preserved).
    pub fn member_by_name(&self, name: &str) -> Option<&CrateInfo> {
        self.members.iter().find(|c| c.name == name)
    }

    /// Absolute paths of every file spliced into the workspace via
    /// `include!(...)` (generated code), across all members. The diagnostic
    /// pipeline uses this set to drop findings anchored within generated files:
    /// generated code participates in analysis but is not a place users can
    /// act on. Only literal / `CARGO_*`-resolvable includes are in the set —
    /// the fast tier runs no build scripts, so `OUT_DIR` output never appears.
    pub fn generated_files(&self) -> impl Iterator<Item = &Path> {
        self.members
            .iter()
            .flat_map(|c| c.generated_files.iter().map(PathBuf::as_path))
    }

    /// Read and parse the given source file with `syn::parse_file`.
    ///
    /// [`Module`] only stores the file *path*, not the parsed AST — that
    /// keeps the whole model `Send + Sync` (a `syn::File` is `Send` but not
    /// `Sync` because `proc-macro2::Span` contains `PhantomData<Rc<()>>`).
    /// Callers that need the AST call this helper on demand and cache as they
    /// see fit (typically a `HashMap<PathBuf, syn::File>` keyed by
    /// `module.file`).
    pub fn parse_file(&self, path: &Path) -> Result<syn::File> {
        let source = std::fs::read_to_string(path)?;
        syn::parse_file(&source).map_err(|e| FastError::Parse {
            path: path.to_path_buf(),
            source: e,
        })
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

    /// Cargo `[features]` declared in this crate's `Cargo.toml`, sorted.
    /// Includes `default` if defined.
    pub fn declared_features(&self) -> &[String] {
        &self.declared_features
    }

    /// Each declared feature mapped to its activation list, as reported by
    /// `cargo metadata` — so the synthesized `foo = ["dep:foo"]` entry for an
    /// implicit optional-dependency feature is included. A feature with an
    /// empty list is a "leaf" (gates code directly); a non-empty list means
    /// the feature forwards to a dependency or another feature ("plumbing" /
    /// "umbrella"), which legitimately never appears in a `#[cfg(feature)]`
    /// gate. Keyed identically to [`Self::declared_features`].
    pub fn feature_values(&self) -> &BTreeMap<String, Vec<String>> {
        &self.feature_values
    }

    /// One [`Target`] per Cargo target (`[lib]`, `[[bin]]`, `[[example]]`,
    /// `[[test]]`, `[[bench]]`, proc-macro lib, `build.rs`), each with its
    /// lean module tree.
    pub fn targets(&self) -> &[Target] {
        &self.targets
    }

    /// `.rs` files under `<manifest_dir>/src/` that aren't reached by any
    /// of this crate's targets' module trees and aren't the `src_path` of
    /// some other target. Useful for module-tree integrity analyses.
    pub fn orphan_files(&self) -> &[PathBuf] {
        &self.orphan_files
    }

    /// Iterate every module in every target, root-first within each target.
    pub fn all_modules(&self) -> impl Iterator<Item = &Module> + '_ {
        self.targets.iter().flat_map(|t| t.root.walk())
    }

    /// Crate names referenced inside this crate's rust-compiling doc-test
    /// fences, unioned over every target's modules. A dep used only in a doc
    /// example is genuinely used (the doc-test won't compile without it), but
    /// doc-test code is a separate compilation unit the rustc IR never sees —
    /// so the dependency lint reads this syntactic signal alongside the
    /// semantic reference graph.
    pub fn doctest_dep_refs(&self) -> std::collections::HashSet<&str> {
        self.all_modules()
            .flat_map(|m| m.doctest_crate_refs.iter().map(String::as_str))
            .collect()
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
                "wl-diagnostic",
                "wl-engine",
                "wl-ir",
                "wl-lints",
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

    #[test]
    fn syntactic_layer_reaches_this_file_and_finds_no_orphans() {
        let model = load_this_workspace();
        let member = model.member_by_name("wl-engine").unwrap();
        // `lib.rs → mod fast → mod metadata` resolves to this very file.
        assert!(
            member
                .all_modules()
                .any(|m| m.file.ends_with("fast/metadata.rs")),
            "the module walk should reach fast/metadata.rs"
        );
        // A clean crate has no unreachable `src/**.rs` files (the dogfooded
        // module-tree lint denies them).
        assert_eq!(member.orphan_files(), &[] as &[std::path::PathBuf]);
    }
}
