//! The resolved tree model: [`Module`] and its walk iterators, the cargo
//! [`Target`]/[`TargetKind`], and the [`Crate`]. Built by `module_tree` during
//! load and queried through [`Workspace`].

use std::path::PathBuf;

use super::use_tree;
use super::{BrokenModDecl, Item, Occurrence, Origin, ResolvedPath, SignatureExposure, Visibility};

/// A module within a crate. Modules form a tree rooted at the crate's `lib.rs`
/// or `main.rs`.
#[derive(Debug, Clone)]
pub struct Module {
    pub name: String,
    pub canonical: ResolvedPath,
    /// Visibility of the `mod foo;` declaration in the parent. Crate roots
    /// (lib.rs / main.rs / proc-macro entry) are always [`Visibility::Public`]
    /// — they're the crate boundary itself, not a `mod` declaration. Used by
    /// downstream lints (visibility, unused-pub) to determine whether items
    /// are externally reachable: an item at a `pub(crate) mod` (or private
    /// `mod`) hop in its path is not part of the crate's public API even
    /// if the item itself is `pub`.
    pub visibility: Visibility,
    pub items: Vec<Item>,
    pub submodules: Vec<Module>,
    /// `use` bindings active in this module's scope (renames resolved to
    /// canonical paths). Populated by Tier 1 during the Tier 2 walk; inline
    /// child modules carry their own bindings independently of their parent.
    pub use_bindings: Vec<use_tree::UseBinding>,
    /// `mod foo;` declarations encountered in this module whose target file
    /// couldn't be resolved (and which don't have an inline body). Surfaces
    /// dangling-module declarations for module-tree integrity analyses.
    pub broken_mod_decls: Vec<BrokenModDecl>,
    /// Feature names referenced via `#[cfg(feature = "...")]` or
    /// `#[cfg_attr(feature = "...", ...)]` on any item declared in this
    /// module (outer attributes only — feature gates inside function
    /// bodies are not extracted here). Deduped, sorted lexicographically.
    pub cfg_features: Vec<String>,
    /// Every reference occurrence in this module — regular-code paths, glob
    /// prefixes, `extern crate`, and macro-body refs — each with its raw
    /// segments, resolved `path`, span, and [`Origin`]. The resolver's primary
    /// reference surface; use [`Module::references`] / [`Module::macro_refs`]
    /// for the resolved paths split by channel.
    pub occurrences: Vec<Occurrence>,
    /// Canonical target prefixes of **public** glob re-exports
    /// (`pub use M::*;`) declared in this module — e.g. `pub use crate::foo::*`
    /// records `crate_code::foo`. A `pub use M::*` re-exports every public item
    /// of `M` into this module's public surface, so [`ReExportIndex`](crate::ReExportIndex) marks
    /// those items as re-export targets (the named-`pub use` exemption,
    /// extended to globs). Private (`use M::*`) globs are not recorded — they
    /// import, they don't re-export.
    pub glob_reexports: Vec<ResolvedPath>,
    /// Type paths that appear in this module's **public signature surface** —
    /// `pub fn` parameter/return types, `pub` field types, trait-impl
    /// associated-type values, type-alias RHSs, trait-item signatures, etc.,
    /// each tagged with the visibility of the exposing item. Aggregated into
    /// [`Workspace::exposed_in_public_signature`](crate::Workspace::exposed_in_public_signature),
    /// which `unused-pub` consults so it never narrows a type that a more-visible
    /// item exposes (which would not compile — E0446 / `private_interfaces`).
    pub signature_exposures: Vec<SignatureExposure>,
    /// Canonical targets of local-fact reference edges contributed by resolver
    /// plugins from this module's items — the Tier-H usage assertions (a strum
    /// derive ⇒ `strum`, `#[serde(with = "m")]` ⇒ `m::{serialize,deserialize}`, …).
    /// Drained into [`Workspace::references_by_crate`](crate::Workspace) at load so they
    /// suppress `unused-deps` / `unused-pub` false positives. Deliberately **not** in
    /// `occurrences`: that keeps them out of the SCIP projection and
    /// [`Module::references`], so the precision gate stays parsed-evidence-only.
    /// `pub(crate)`: an internal resolution detail, not part of the published surface.
    pub(crate) fact_references: Vec<ResolvedPath>,
    /// Provenance for every resolver-plugin [`Fact`](crate::plugins) produced from
    /// this module's items (the builder-attr exposures today). Aggregated into
    /// [`Workspace::fact_provenance`](crate::Workspace) for a future `--explain`;
    /// inert otherwise (it never affects whether a finding fires). `pub(crate)`:
    /// an internal provenance detail, not part of the published model surface.
    pub(crate) fact_provenance: Vec<crate::plugins::ProvenancedFact>,
    /// File backing this module, if any. `None` for inline `mod foo { ... }`
    /// blocks whose file is the parent.
    pub file: Option<PathBuf>,
    /// Absolute paths of files spliced into this module via `include!(...)`
    /// (generated code — e.g. a build script's `OUT_DIR` output). The included
    /// items live directly in this module (an `include!` is not a submodule), so
    /// these files are *not* `Module::file` of any node; recording them here lets
    /// the module-tree integrity check treat them as reached and the diagnostic
    /// pipeline mark findings anchored in them as generated. Empty for the
    /// overwhelmingly common no-`include!` module.
    pub generated_files: Vec<PathBuf>,
    /// Crate names referenced inside rust-compiling code fences in this file's
    /// doc comments (`///` / `//!`). Populated only on the file-backed module
    /// (see [`module_tree::build_module_from_file`]); empty for inline
    /// submodules, which share the file. Feeds the dependency lint **only**
    /// (via [`Workspace::doctest_dep_refs`]) — a dep used solely in a doc-test
    /// example is still genuinely used, but doc-test code is a separate
    /// compilation unit, so these refs are deliberately kept out of the
    /// occurrence graph that `unused-pub` / the SCIP projection read.
    pub(crate) doctest_crate_refs: std::collections::HashSet<String>,
}

impl Module {
    /// Recursively iterate this module and all its submodules, depth-first,
    /// root first. The most common entry point for callers that need to
    /// scan every module under a crate target.
    pub fn walk(&self) -> impl Iterator<Item = &Module> + '_ {
        ModuleWalk::new(self)
    }

    /// Iterate every `(module, item)` pair under this module's subtree.
    /// Preserves the enclosing module so callers can consult its
    /// `canonical`, `file`, etc. without a second lookup.
    pub fn walk_items(&self) -> impl Iterator<Item = (&Module, &Item)> + '_ {
        self.walk()
            .flat_map(|m| m.items.iter().map(move |i| (m, i)))
    }

    /// Iterate every `(module, use_binding)` pair under this module's
    /// subtree. Mirrors [`Module::walk_items`] for `use` declarations.
    pub fn walk_use_bindings(&self) -> impl Iterator<Item = (&Module, &use_tree::UseBinding)> + '_ {
        self.walk()
            .flat_map(|m| m.use_bindings.iter().map(move |b| (m, b)))
    }

    /// Resolved paths referenced from this module's regular code, glob imports,
    /// and `extern crate` declarations — parsed evidence only (every [`Origin`]
    /// except `Macro`). Tier-H assertion refs live on `fact_references`, not here, so
    /// this surface stays parsed-evidence-only. Unresolved occurrences are skipped.
    pub fn references(&self) -> impl Iterator<Item = &ResolvedPath> + '_ {
        self.occurrences
            .iter()
            .filter(|o| !matches!(o.origin, Origin::Macro))
            .filter_map(|o| o.path.as_ref())
    }

    /// Resolved paths referenced inside macro bodies (`Origin::Macro`).
    /// Unresolved occurrences are skipped.
    pub fn macro_refs(&self) -> impl Iterator<Item = &ResolvedPath> + '_ {
        self.occurrences
            .iter()
            .filter(|o| o.origin == Origin::Macro)
            .filter_map(|o| o.path.as_ref())
    }
}

/// Kind of a Cargo target. Library crate-types (`lib`/`rlib`/`dylib`/
/// `cdylib`/`staticlib`) are coalesced into [`TargetKind::Lib`] since
/// downstream consumers rarely distinguish them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TargetKind {
    Lib,
    ProcMacro,
    Bin,
    Example,
    Test,
    Bench,
    BuildScript,
}

/// One Cargo target inside a crate: a `[lib]`, `[[bin]]`, `[[example]]`,
/// `[[test]]`, `[[bench]]`, proc-macro library, or `build.rs`. Each target
/// has its own module tree, since cargo compiles each as a separate crate.
#[derive(Debug, Clone)]
pub struct Target {
    pub kind: TargetKind,
    /// Target name from `Cargo.toml` (or auto-derived for path-discovered
    /// targets).
    pub name: String,
    /// Absolute path to the target's root source file (e.g.
    /// `…/src/lib.rs`, `…/src/main.rs`, `…/build.rs`,
    /// `…/tests/integration.rs`).
    pub src_path: PathBuf,
    /// Module tree rooted at `src_path`. The root module's `canonical` is
    /// the parent crate's code-form name — even for non-lib targets — so
    /// cross-crate references (e.g. `serde::Foo`) inside a test attribute
    /// to the parent crate's reference set without polluting it with
    /// synthetic-root paths.
    pub root: Module,
}

impl Target {
    /// Recursively iterate every module in this target's tree, root first.
    pub fn all_modules(&self) -> impl Iterator<Item = &Module> + '_ {
        self.root.walk()
    }
}

/// A crate — either a workspace member or an external dependency. External
/// crates are represented sparsely (name + version + declared deps); only
/// workspace members have a full module tree.
#[derive(Debug, Clone)]
pub struct Crate {
    pub name: String,
    pub version: String,
    pub manifest_dir: PathBuf,
    pub is_workspace_member: bool,
    /// One [`Target`] per Cargo target (`[lib]`, `[[bin]]`, `[[example]]`,
    /// `[[test]]`, `[[bench]]`, proc-macro lib, `build.rs`). For external
    /// crates this is empty.
    pub targets: Vec<Target>,
    /// `.rs` files under `<manifest_dir>/src/` that aren't reached by any
    /// of this crate's targets' module trees and aren't the `src_path` of
    /// some other target. Useful for module-tree integrity analyses and
    /// for tools that scan source independently of the resolved tree.
    pub orphan_files: Vec<PathBuf>,
    /// Absolute paths of files spliced into this crate via `include!(...)`
    /// (generated code), unioned across every target's module tree (each
    /// module records its own in [`Module::generated_files`]). Consumers treat
    /// these files as generated: reachable for module-tree integrity, and
    /// exempt from findings anchored within them. Empty for crates with no
    /// resolved `include!`.
    pub generated_files: Vec<PathBuf>,
    /// Cargo `[features]` declared in this crate's `Cargo.toml`. Includes
    /// `default` if defined. Activation lists are not retained — only the
    /// set of feature names.
    pub declared_features: Vec<String>,
    /// Each declared feature mapped to its activation list, as reported by
    /// `cargo metadata` — so the synthesized `foo = ["dep:foo"]` entry for an
    /// implicit optional-dependency feature is included. A feature with an
    /// empty list is a "leaf" (gates code directly); a non-empty list means
    /// the feature forwards to a dependency or another feature ("plumbing" /
    /// "umbrella"), which legitimately never appears in a `#[cfg(feature)]`
    /// gate. Keyed identically to [`Self::declared_features`].
    pub feature_values: std::collections::BTreeMap<String, Vec<String>>,
    /// Parsed `Cargo.toml`. Prefer this over re-parsing the file from disk
    /// when you need section enumeration or byte-located dep lines for
    /// structural rewrites.
    pub manifest: crate::manifest::Manifest,
}

impl Crate {
    /// The crate's primary unit — preferring a library or proc-macro
    /// target, falling back to the first binary. `None` for crates with
    /// no targets at all (typically external/non-member entries).
    ///
    /// Most consumers that historically walked `krate.root` want this:
    /// analyses targeting the cross-crate API surface (public items,
    /// visibility, re-exports) care about the lib surface, not the
    /// test/bench/build-script trees.
    pub fn lib_or_main(&self) -> Option<&Target> {
        self.targets
            .iter()
            .find(|t| matches!(t.kind, TargetKind::Lib | TargetKind::ProcMacro))
            .or_else(|| self.targets.iter().find(|t| t.kind == TargetKind::Bin))
    }

    /// Iterate targets of a specific [`TargetKind`].
    pub fn targets_of_kind(&self, kind: TargetKind) -> impl Iterator<Item = &Target> + '_ {
        self.targets.iter().filter(move |t| t.kind == kind)
    }

    /// Iterate every module in every target, root-first within each target.
    /// Use this when a consumer needs the whole crate's surface — e.g.
    /// scanning `cfg_features` across every target kind, not just the
    /// primary lib.
    pub fn all_modules(&self) -> impl Iterator<Item = &Module> + '_ {
        self.targets.iter().flat_map(|t| t.root.walk())
    }

    /// Iterate items in the crate's primary unit (lib_or_main).
    /// Test / build-script / bin-not-primary items are *not* included —
    /// they're not part of the cross-crate API surface most consumers
    /// reason about. Use [`Crate::all_items`] for full coverage.
    pub fn items(&self) -> impl Iterator<Item = &Item> + '_ {
        self.lib_or_main()
            .into_iter()
            .flat_map(|t| t.root.walk_items().map(|(_, i)| i))
    }

    /// Items in *every* target. Rarely needed — most consumers want
    /// [`Crate::items`].
    pub fn all_items(&self) -> impl Iterator<Item = &Item> + '_ {
        self.targets
            .iter()
            .flat_map(|t| t.root.walk_items().map(|(_, i)| i))
    }

    /// Iterate items whose visibility is reachable outside the crate
    /// (currently: `Public` only — `pub(crate)` is intra-crate). Restricted
    /// to the primary unit; tests/build-scripts don't expose a stable API.
    pub fn pub_items(&self) -> impl Iterator<Item = &Item> + '_ {
        self.items()
            .filter(|i| matches!(i.visibility, Visibility::Public))
    }

    /// In-code form of the crate name (Cargo hyphens replaced with `_`).
    ///
    /// `Crate::name` is the Cargo form (`data-models`), but source code
    /// references the crate as `data_models::...` — most cross-crate
    /// resolver indexes (e.g. [`Workspace::references_from_crate`](crate::Workspace::references_from_crate)) key on
    /// the code form, so callers should prefer this method over hand-rolling
    /// `name.replace('-', "_")`.
    pub fn code_name(&self) -> String {
        self.name.replace('-', "_")
    }

    /// Parsed `Cargo.toml` for this crate. Use this in preference to
    /// re-parsing the file from disk.
    pub fn manifest(&self) -> &crate::manifest::Manifest {
        &self.manifest
    }

    /// All declared dependencies across `[dependencies]`,
    /// `[dev-dependencies]`, and `[build-dependencies]`. Delegates to
    /// [`crate::manifest::Manifest::declared_deps`].
    pub fn declared_deps(&self) -> impl Iterator<Item = crate::manifest::DeclaredDep> + '_ {
        self.manifest.declared_deps()
    }
}

/// Recursive iterator over modules in a tree, yielding the root first
/// then descending into submodules depth-first. The public entry points
/// are [`Module::walk`], [`Module::walk_items`], and
/// [`Module::walk_use_bindings`].
struct ModuleWalk<'a> {
    stack: Vec<&'a Module>,
}

impl<'a> ModuleWalk<'a> {
    fn new(root: &'a Module) -> Self {
        Self { stack: vec![root] }
    }
}

impl<'a> Iterator for ModuleWalk<'a> {
    type Item = &'a Module;

    fn next(&mut self) -> Option<Self::Item> {
        let module = self.stack.pop()?;
        for sub in module.submodules.iter().rev() {
            self.stack.push(sub);
        }
        Some(module)
    }
}
