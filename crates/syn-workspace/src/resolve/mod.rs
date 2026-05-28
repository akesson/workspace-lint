//! Resolved workspace model and the name-resolution pipeline that builds it.
//!
//! The pipeline runs in three tiers, each adding precision:
//!
//! - [`use_tree`] — Tier 1: per-file `use` and `use ... as ...` tracking.
//! - [`module_tree`] — Tier 2: cross-file modules (`mod foo;`, `#[path]`).
//! - [`re_export`] — Tier 2.5: `pub use` chain following.
//!
//! Each tier produces structures that the next consumes; the entry point is
//! [`Workspace::load`], which orchestrates all three.

pub mod module_tree;
pub mod re_export;
pub mod use_tree;

use std::path::{Path, PathBuf};

pub type Result<T> = std::result::Result<T, Error>;

/// Errors produced while loading or resolving a workspace.
///
/// `#[non_exhaustive]`: new variants may be added in minor versions.
/// Match arms should include a catch-all (`_ => ...`).
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// Failed to read or parse a `Cargo.toml`. The `source` chain holds
    /// the original I/O / parse error.
    Manifest {
        path: PathBuf,
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// Failed to parse a Rust source file. The `source` is the original
    /// [`syn::Error`].
    Parse { path: PathBuf, source: syn::Error },
    /// Generic I/O error during workspace traversal.
    Io(std::io::Error),
    /// `cargo metadata` itself failed (e.g. invalid workspace,
    /// unresolvable dep tree).
    Metadata(cargo_metadata::Error),
}

impl Error {
    /// Convenience constructor for [`Error::Manifest`]. Wraps the source
    /// error in a `Box`.
    pub fn manifest(
        path: impl Into<PathBuf>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::Manifest {
            path: path.into(),
            source: Box::new(source),
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Manifest { path, source } => {
                write!(f, "manifest error in {}: {source}", path.display())
            }
            Self::Parse { path, source } => {
                write!(f, "parse error in {}: {source}", path.display())
            }
            Self::Io(err) => write!(f, "I/O error: {err}"),
            Self::Metadata(err) => write!(f, "cargo metadata: {err}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Manifest { source, .. } => Some(&**source),
            Self::Parse { source, .. } => Some(source),
            Self::Io(err) => Some(err),
            Self::Metadata(err) => Some(err),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<cargo_metadata::Error> for Error {
    fn from(err: cargo_metadata::Error) -> Self {
        Self::Metadata(err)
    }
}

/// A non-fatal issue encountered while loading the workspace.
///
/// The resolver records these and continues; the caller decides whether
/// to surface, log, or ignore them. The library itself never writes to
/// stderr.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum LoadWarning {
    /// An auxiliary target (test, example, bench, or build script) failed
    /// to parse. The primary lib/bin/proc-macro target's failure is fatal
    /// and propagates as `Err`; only auxiliary targets degrade to a
    /// warning.
    TargetParseFailed {
        /// Target name from `Cargo.toml` (or auto-derived name for
        /// path-discovered targets).
        target: String,
        /// Source file the parse was attempted on.
        path: PathBuf,
        /// Stringified error from the parse attempt.
        message: String,
    },
}

impl std::fmt::Display for LoadWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TargetParseFailed {
                target,
                path,
                message,
            } => write!(
                f,
                "skipping target {target} ({}): {message}",
                path.display()
            ),
        }
    }
}

/// Configuration for [`Workspace::load_with_options`].
///
/// All fields have sensible defaults; reach for this struct only when
/// you need to override one — most callers can stick with
/// [`Workspace::load`].
#[derive(Debug, Clone)]
pub struct LoadOptions {
    /// Crate names recognized as the "marker" crate that owns the
    /// `expansion_uses!` macro. A `<crate>::expansion_uses!(...)`
    /// invocation is treated as a Layer 2 annotation iff the leading
    /// segment matches one of these names. The unqualified form
    /// (`expansion_uses!(...)` with no prefix) always matches.
    ///
    /// Default: `["workspace_syn", "syn_workspace_marker"]` — kept
    /// backward-compatible with the original hardcoded list.
    pub marker_crates: Vec<String>,
}

impl Default for LoadOptions {
    fn default() -> Self {
        Self {
            marker_crates: vec!["workspace_syn".into(), "syn_workspace_marker".into()],
        }
    }
}

/// A canonical, fully-qualified path to an item, after rename and re-export
/// resolution.
///
/// Examples:
/// - `serde::Deserialize` — external crate item
/// - `data_models::user::User` — workspace-crate item at its definition site
/// - `apps_dashboard::main` — entry point
///
/// The first segment is always a crate name (workspace or external). Segments
/// after that follow the canonical module path *at the definition site*, not
/// at any re-export location.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ResolvedPath {
    segments: Vec<String>,
}

impl ResolvedPath {
    /// Construct from a sequence of segments. The first must be a crate name.
    pub fn new<I, S>(segments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            segments: segments.into_iter().map(Into::into).collect(),
        }
    }

    /// Parse a `::`-separated path written by a human (config files, CLI
    /// arguments). Splits on `::`, trims each segment, and drops empties.
    /// Normalizes the leading crate segment from cargo form (`data-models`)
    /// to in-code form (`data_models`) so the result lines up with the
    /// canonical paths the resolver stores.
    pub fn from_user_str(s: &str) -> Self {
        let mut segments: Vec<String> = s
            .split("::")
            .map(|seg| seg.trim().to_string())
            .filter(|seg| !seg.is_empty())
            .collect();
        if let Some(first) = segments.first_mut() {
            *first = first.replace('-', "_");
        }
        Self { segments }
    }

    /// All segments, including the leading crate name.
    pub fn segments(&self) -> &[String] {
        &self.segments
    }

    /// Crate name (first segment).
    pub fn crate_name(&self) -> Option<&str> {
        self.segments.first().map(String::as_str)
    }

    /// Render as `crate::module::Item`.
    pub fn display(&self) -> String {
        self.segments.join("::")
    }
}

impl std::fmt::Display for ResolvedPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.display())
    }
}

/// Rust item visibility, normalized to syn-workspace's vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    /// `pub`
    Public,
    /// `pub(crate)`
    PubCrate,
    /// `pub(super)`
    PubSuper,
    /// `pub(in path::to::module)`
    PubIn,
    /// Bare item (default private to the defining module).
    Private,
}

impl Visibility {
    /// Map a `syn::Visibility` to this crate's normalized vocabulary.
    pub fn from_syn(v: &syn::Visibility) -> Self {
        match v {
            syn::Visibility::Public(_) => Self::Public,
            syn::Visibility::Restricted(r) => {
                if r.path.is_ident("crate") {
                    Self::PubCrate
                } else if r.path.is_ident("super") {
                    Self::PubSuper
                } else {
                    Self::PubIn
                }
            }
            syn::Visibility::Inherited => Self::Private,
        }
    }
}

/// Kind of a declared item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemKind {
    Fn,
    Struct,
    Enum,
    Union,
    Trait,
    TypeAlias,
    Const,
    Static,
    Module,
    Macro,
    Impl,
    Use,
    ExternCrate,
}

impl ItemKind {
    /// True for items that define a named API surface (`Fn`, `Struct`,
    /// `Enum`, `Union`, `Trait`, `TypeAlias`, `Const`, `Static`, `Macro`).
    /// False for `Module`, `Impl`, `Use`, `ExternCrate` — those are
    /// containers, declarations, or non-named blocks rather than definitions
    /// that participate in cross-crate API consumption.
    pub fn is_definition(self) -> bool {
        matches!(
            self,
            Self::Fn
                | Self::Struct
                | Self::Enum
                | Self::Union
                | Self::Trait
                | Self::TypeAlias
                | Self::Const
                | Self::Static
                | Self::Macro
        )
    }
}

impl std::fmt::Display for ItemKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Fn => "fn",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Union => "union",
            Self::Trait => "trait",
            Self::TypeAlias => "type",
            Self::Const => "const",
            Self::Static => "static",
            Self::Module => "mod",
            Self::Macro => "macro",
            Self::Impl => "impl",
            Self::Use => "use",
            Self::ExternCrate => "extern crate",
        };
        f.write_str(s)
    }
}

/// A single declared item in a module.
#[derive(Debug, Clone)]
pub struct Item {
    pub name: String,
    pub kind: ItemKind,
    pub visibility: Visibility,
    /// Canonical path at the definition site (crate-relative, prefixed with
    /// the crate name).
    pub canonical: ResolvedPath,
    /// File and line where the item is declared. `None` for synthesized
    /// items (e.g. crate roots).
    pub source: Option<SourceSpan>,
    /// Byte range of the `pub` keyword itself, when [`Self::visibility`] is
    /// [`Visibility::Public`]. Lets structural fixes (e.g. visibility
    /// tighteners) rewrite the keyword precisely without scanning past
    /// preceding doc comments and attributes. `None` for non-public items,
    /// macros (which have no `pub` token), or spans the resolver couldn't
    /// pin to byte offsets.
    pub vis_byte_range: Option<std::ops::Range<u32>>,
}

/// File location of a syntactic element.
///
/// `byte_range` covers the entire item (from its first attribute or `pub`
/// keyword through the closing brace or semicolon) when the underlying
/// span carried byte offsets. `None` means the span is synthetic or the
/// resolver couldn't determine the range — `file`/`line` may still be
/// useful for diagnostic messages in that case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSpan {
    pub file: PathBuf,
    pub line: u32,
    pub column: u32,
    /// Inclusive-start, exclusive-end byte range within `file`. `None`
    /// for synthetic spans.
    pub byte_range: Option<std::ops::Range<u32>>,
}

impl SourceSpan {
    /// True iff `byte_range` is populated. Callers that drive structural
    /// fixes by byte-range replacement gate on this.
    pub fn has_byte_range(&self) -> bool {
        self.byte_range.is_some()
    }
}

/// A `mod foo;` declaration that didn't resolve to a backing file.
///
/// Recorded so consumers can flag the mismatch — typically a rename
/// that left the declaration dangling, or a `#[cfg_attr(..., path = ...)]`
/// form syn-workspace doesn't yet evaluate.
#[derive(Debug, Clone)]
pub struct BrokenModDecl {
    /// The `mod` name (`foo` in `mod foo;`).
    pub name: String,
    /// The file containing the failing declaration.
    pub declared_in: PathBuf,
    /// 1-indexed line number of the `mod` keyword within `declared_in`.
    pub line: u32,
}

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
    /// Canonical paths referenced inside `macro_rules!` bodies declared in
    /// this module (Layer 1 autodetect). Conservative: any multi-segment
    /// path appearing in the macro RHS gets resolved through the macro's
    /// defining scope and recorded. Useful for any analysis that wants to
    /// know which items might be reachable through a workspace-owned macro
    /// without expanding macros for real.
    pub macro_implicit_refs: Vec<ResolvedPath>,
    /// Canonical paths referenced from this module's regular code (function
    /// bodies, type signatures, attribute paths). Distinct from
    /// `use_bindings` (which records what `use` statements bring into scope)
    /// and from `macro_implicit_refs` (which records paths inside
    /// `macro_rules!` bodies). Populated by token-scanning each non-`use`,
    /// non-`macro_rules!` item with use-binding substitution applied to the
    /// leading segment.
    ///
    /// Useful for cross-crate reference graphs without paying the cost of
    /// a full semantic index. Lints, dependency analyzers, and architectural
    /// checks all consume this in different ways.
    pub references: Vec<ResolvedPath>,
    /// File backing this module, if any. `None` for inline `mod foo { ... }`
    /// blocks whose file is the parent.
    pub file: Option<PathBuf>,
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
    /// Cargo `[features]` declared in this crate's `Cargo.toml`. Includes
    /// `default` if defined. Activation lists are not retained — only the
    /// set of feature names.
    pub declared_features: Vec<String>,
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
    /// resolver indexes (e.g. [`Workspace::references_from_crate`]) key on
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

/// The top-level resolved workspace.
#[derive(Debug, Clone)]
pub struct Workspace {
    crates: Vec<Crate>,
    root: PathBuf,
    /// Parsed root `Cargo.toml`. Carries the `[workspace.dependencies]`
    /// table that consumers like centralized-deps checks query, plus the
    /// raw source bytes for comment-based directive scanners.
    root_manifest: crate::manifest::Manifest,
    re_exports: re_export::ReExportIndex,
    /// Macro implicit references partitioned by defining crate (code name).
    /// Built eagerly at load time by unioning every module's
    /// `macro_implicit_refs` per crate. Used by
    /// [`Workspace::macro_implicit_refs_for`] to compute per-target-crate
    /// reachability-narrowed sets — a macro defined in crate B only
    /// contributes to the set for crate A if A references B (or B == A).
    macro_refs_by_crate: std::collections::HashMap<String, std::collections::HashSet<ResolvedPath>>,
    /// External-macro references registered via
    /// [`Workspace::register_external_macro_uses`]. Treated as
    /// workspace-wide because we can't tell from `cargo_metadata` which
    /// crates actually invoke an external macro — broadcasting to all
    /// keeps the model conservative for that specific shape.
    external_macro_refs: std::collections::HashSet<ResolvedPath>,
    /// Per-crate set of canonical paths referenced from that crate's regular
    /// code (combines `use` bindings + the `Module.references` set). Keyed
    /// by the crate's code name (Cargo-form hyphens replaced with '_').
    /// Built once at load time so consumers don't have to re-walk the tree.
    references_by_crate: std::collections::HashMap<String, std::collections::HashSet<ResolvedPath>>,
    /// Reverse index: for each canonical path, the set of code-form crate
    /// names that reference it. Built from `references_by_crate` with each
    /// path passed through the `pub use` chain in `re_exports`. Same
    /// referrer may appear because intra-crate refs are retained — callers
    /// that want "cross-crate only" filter on `path.crate_name() !=
    /// referrer`. Pre-computed so the re-export resolution runs once
    /// regardless of how many consumers query it.
    canonical_refs_by_path:
        std::collections::HashMap<ResolvedPath, std::collections::HashSet<String>>,
    /// Non-fatal issues collected during the load (typically auxiliary
    /// targets that failed to parse). The library never prints these;
    /// callers decide whether to surface, log, or ignore them.
    warnings: Vec<LoadWarning>,
}

impl Workspace {
    /// Load and resolve a workspace at the given root directory, with
    /// default options. See [`Workspace::load_with_options`] for the
    /// configurable form.
    pub fn load(root: impl AsRef<Path>) -> Result<Self> {
        Self::load_with_options(root, LoadOptions::default())
    }

    /// Load and resolve a workspace, configured via [`LoadOptions`].
    ///
    /// Builds the full model in one pass: workspace discovery via
    /// `cargo_metadata`, per-crate module-tree assembly (Tier 2) which
    /// threads Tier 1 use-bindings into each [`Module`], and a
    /// workspace-wide `pub use` chain index (Tier 2.5).
    pub fn load_with_options(root: impl AsRef<Path>, opts: LoadOptions) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let (root_manifest, crates, warnings) =
            crate::walk::load_members(&root, &opts.marker_crates)?;
        let re_exports = re_export::ReExportIndex::build(&crates);
        let mut macro_refs_by_crate: std::collections::HashMap<
            String,
            std::collections::HashSet<ResolvedPath>,
        > = std::collections::HashMap::new();
        let mut references_by_crate: std::collections::HashMap<
            String,
            std::collections::HashSet<ResolvedPath>,
        > = std::collections::HashMap::new();
        for krate in &crates {
            if !krate.is_workspace_member {
                continue;
            }
            let code_name = krate.code_name();
            let macro_entry = macro_refs_by_crate.entry(code_name.clone()).or_default();
            let entry = references_by_crate.entry(code_name.clone()).or_default();
            // Walk every target (lib/bin/example/test/bench/build-script).
            // Each target's tree was built with the parent crate's code_name
            // as canonical root, so cross-crate references (e.g.
            // `serde::Foo` inside an integration test) attribute correctly
            // to the parent. Intra-target paths like `crate::helpers::foo`
            // become `parent_crate::helpers::foo` — self-references that
            // consumers filter out (a dep analyzer ignores them because
            // they don't match a Cargo.toml dep; a visibility analyzer
            // ignores them because they're same-crate; etc.).
            for target in &krate.targets {
                collect_macro_implicit_refs(&target.root, macro_entry);
                collect_module_references(&target.root, entry);
            }
        }
        let mut canonical_refs_by_path: std::collections::HashMap<
            ResolvedPath,
            std::collections::HashSet<String>,
        > = std::collections::HashMap::new();
        for (referring_crate, refs) in &references_by_crate {
            for path in refs {
                let canonical = re_exports.canonical(path);
                canonical_refs_by_path
                    .entry(canonical)
                    .or_default()
                    .insert(referring_crate.clone());
            }
        }
        Ok(Self {
            crates,
            root,
            root_manifest,
            re_exports,
            macro_refs_by_crate,
            external_macro_refs: std::collections::HashSet::new(),
            references_by_crate,
            canonical_refs_by_path,
            warnings,
        })
    }

    /// Non-fatal issues collected during [`Workspace::load`]. Typically
    /// auxiliary targets (test/example/bench/build-script) that failed
    /// to parse — the primary lib/bin/proc-macro target's failure
    /// propagates as `Err` rather than landing here.
    ///
    /// Empty when nothing went wrong. Callers decide whether to log,
    /// print, or ignore the entries; this library never writes to stderr.
    pub fn warnings(&self) -> &[LoadWarning] {
        &self.warnings
    }

    /// Parsed root `Cargo.toml`. Carries the `[workspace.dependencies]`
    /// table (queried by centralized-dep analyses) and the raw source
    /// bytes (useful for comment-based directive scanners).
    pub fn root_manifest(&self) -> &crate::manifest::Manifest {
        &self.root_manifest
    }

    /// Register canonical paths that an external macro's expansion is
    /// known to reference. Each call appends; the underlying
    /// [`std::collections::HashSet`] dedupes. Typically called once after
    /// [`Workspace::load`], passing entries discovered by the caller
    /// (e.g. parsed from a config file, hardcoded, or learned at runtime).
    ///
    /// External-macro refs are treated as workspace-wide (broadcast to
    /// every crate) because `cargo_metadata` can't tell us which workspace
    /// crates actually invoke a given external macro. Callers that want
    /// per-crate scoping should track their own per-crate sets.
    pub fn register_external_macro_uses<I>(&mut self, paths: I)
    where
        I: IntoIterator<Item = ResolvedPath>,
    {
        self.external_macro_refs.extend(paths);
    }

    /// All workspace member crates plus referenced external crates.
    pub fn crates(&self) -> &[Crate] {
        &self.crates
    }

    /// Just the workspace member crates.
    pub fn members(&self) -> impl Iterator<Item = &Crate> {
        self.crates.iter().filter(|c| c.is_workspace_member)
    }

    /// Each workspace member paired with its primary unit (lib / proc-macro /
    /// main bin). Members without a primary target — proc-macro-less binaries
    /// without a `[[bin]]` entry, etc. — are skipped. The pair iterator
    /// subsumes the common "for member; if let Some(target) = lib_or_main"
    /// ladder.
    pub fn primary_units(&self) -> impl Iterator<Item = (&Crate, &Target)> + '_ {
        self.members()
            .filter_map(|c| c.lib_or_main().map(|t| (c, t)))
    }

    /// Look up a workspace member by its Cargo-form name (the value users
    /// write in `Cargo.toml`, hyphens preserved).
    pub fn member_by_name(&self, name: &str) -> Option<&Crate> {
        self.members().find(|c| c.name == name)
    }

    /// Look up a workspace member by its in-code form name (hyphens replaced
    /// with `_` — the form that appears as the leading segment of canonical
    /// paths).
    pub fn member_by_code_name(&self, code_name: &str) -> Option<&Crate> {
        self.members().find(|c| c.code_name() == code_name)
    }

    /// Workspace root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Read and parse the given source file with `syn::parse_file`.
    ///
    /// `Module` only stores the file *path*, not the parsed AST — that
    /// keeps the whole workspace model `Send + Sync` (a `syn::File` is
    /// `Send` but not `Sync` because `proc-macro2::Span` contains
    /// `PhantomData<Rc<()>>`). Callers that need the AST call this
    /// helper on demand and cache as they see fit (typically a
    /// `HashMap<PathBuf, syn::File>` keyed by `module.file`).
    pub fn parse_file(&self, path: &Path) -> Result<syn::File> {
        let source = std::fs::read_to_string(path)?;
        syn::parse_file(&source).map_err(|e| Error::Parse {
            path: path.to_path_buf(),
            source: e,
        })
    }

    /// Resolve a path through any `pub use` re-export chain to its canonical
    /// definition site. Returns the path unchanged if no chain applies.
    pub fn resolve_canonical(&self, path: &ResolvedPath) -> ResolvedPath {
        self.re_exports.canonical(path)
    }

    /// Borrow the underlying re-export index — useful for callers that
    /// need to enumerate all known re-export edges.
    pub fn re_exports(&self) -> &re_export::ReExportIndex {
        &self.re_exports
    }

    /// Returns `true` if `path` names an item in a crate that publishes a
    /// stable external API (a library or proc-macro), and every `mod` hop
    /// from the crate root down to (but not including) the item's own name
    /// is declared `pub mod` — i.e. the item is reachable from an external
    /// consumer through ordinary path resolution.
    ///
    /// Used by structural-fix lints (visibility, unused-pub) to refuse
    /// narrowing items that form part of a published crate's public API
    /// even when no in-workspace consumer references them: external
    /// consumers of a library crate live outside the resolver's view.
    ///
    /// Returns `false` for: items whose owning crate isn't a workspace
    /// member, items in a `[[bin]]`-only crate (binaries don't publish an
    /// API), items in non-primary targets (test/example/build-script),
    /// items the resolver couldn't walk to, and items inside a private or
    /// `pub(crate)` module hop.
    pub fn is_externally_reachable(&self, path: &ResolvedPath) -> bool {
        let segments = path.segments();
        // Need at least `[crate_name, item_name]` to talk about reachability.
        if segments.len() < 2 {
            return false;
        }
        let Some(krate) = self.member_by_code_name(&segments[0]) else {
            return false;
        };
        let Some(target) = krate.lib_or_main() else {
            return false;
        };
        // Only lib / proc-macro publish a stable API surface. Bin targets
        // don't expose items to external consumers, so pub items inside a
        // binary crate aren't "reachable from outside" in any meaningful
        // sense — the visibility lint should still suggest narrowing them.
        if !matches!(target.kind, TargetKind::Lib | TargetKind::ProcMacro) {
            return false;
        }
        // Walk every intermediate module hop (skip the crate-root segment
        // and the item name itself). Any non-Public hop breaks reachability.
        let intermediate = &segments[1..segments.len() - 1];
        let mut module = &target.root;
        for seg in intermediate {
            let Some(child) = module.submodules.iter().find(|m| m.name == *seg) else {
                return false;
            };
            if child.visibility != Visibility::Public {
                return false;
            }
            module = child;
        }
        true
    }

    /// Canonical paths reachable through macro expansions that could
    /// plausibly affect items inside `target_crate`. Built per call by
    /// unioning:
    ///
    /// 1. The target crate's own macros (intra-crate macros may reach
    ///    intra-crate items through expansion).
    /// 2. Macros from every workspace crate that references `target_crate`
    ///    — those are the crates whose code could invoke a macro whose body
    ///    points back at `target_crate`'s items.
    /// 3. External-macro entries registered via
    ///    [`Workspace::register_external_macro_uses`] (broadcast to every
    ///    target crate because we can't infer per-crate invocation).
    ///
    /// Reachability-narrowed: a macro body in an unrelated crate does not
    /// contribute. Useful for any consumer that needs to avoid attributing
    /// macro-mediated references to the wrong crate.
    pub fn macro_implicit_refs_for(
        &self,
        target_crate: &Crate,
    ) -> std::collections::HashSet<ResolvedPath> {
        let target_code = target_crate.code_name();
        let mut result = self.external_macro_refs.clone();
        if let Some(refs) = self.macro_refs_by_crate.get(&target_code) {
            result.extend(refs.iter().cloned());
        }
        for (referring_crate, refs) in &self.references_by_crate {
            if referring_crate == &target_code {
                continue;
            }
            let references_target = refs
                .iter()
                .any(|p| p.crate_name() == Some(target_code.as_str()));
            if references_target
                && let Some(macro_refs) = self.macro_refs_by_crate.get(referring_crate)
            {
                result.extend(macro_refs.iter().cloned());
            }
        }
        result
    }

    /// Set of canonical paths referenced from the named crate's regular
    /// code (function bodies, type signatures, etc.) plus its `use`
    /// declarations. `crate_name` is the in-code form (hyphens replaced
    /// with `_`).
    ///
    /// Prefer [`Workspace::references_from_crate`] when you have a
    /// [`Crate`] in hand — it handles the code-name conversion for you.
    ///
    /// Returns `None` if the crate is not a workspace member or the
    /// resolver couldn't load source for it.
    pub fn references_from(
        &self,
        crate_name: &str,
    ) -> Option<&std::collections::HashSet<ResolvedPath>> {
        self.references_by_crate.get(crate_name)
    }

    /// Same as [`Workspace::references_from`] but takes a [`Crate`] and
    /// applies the Cargo→code name conversion automatically.
    pub fn references_from_crate(
        &self,
        krate: &Crate,
    ) -> Option<&std::collections::HashSet<ResolvedPath>> {
        self.references_by_crate.get(&krate.code_name())
    }

    /// Iterator over every `(referring_crate, canonical_path)` reference
    /// pair across the workspace. Useful for building reverse indexes (e.g.
    /// "which crates reference symbol X?").
    pub fn iter_references(&self) -> impl Iterator<Item = (&str, &ResolvedPath)> {
        self.references_by_crate
            .iter()
            .flat_map(|(crate_name, refs)| refs.iter().map(move |r| (crate_name.as_str(), r)))
    }

    /// Like [`Workspace::iter_references`] but each path is already passed
    /// through the `pub use` chain in [`Workspace::re_exports`]. Yields one
    /// `(referring_crate, canonical_path)` pair per (referrer, canonical)
    /// combination — the index dedupes referrers, so two `use` statements
    /// from the same crate pointing at the same canonical produce one
    /// pair.
    ///
    /// Includes intra-crate referrers (a crate's own use of its own item).
    /// Callers that want cross-crate-only filter on
    /// `canonical.crate_name() != referring`.
    pub fn iter_canonical_references(&self) -> impl Iterator<Item = (&str, &ResolvedPath)> + '_ {
        self.canonical_refs_by_path
            .iter()
            .flat_map(|(path, crates)| crates.iter().map(move |c| (c.as_str(), path)))
    }

    /// Set of code-form crate names that reference `canonical` (after
    /// `pub use` chain resolution). `None` means no recorded reference at
    /// all. The returned set may include `canonical.crate_name()` itself
    /// when the defining crate references its own item.
    pub fn referring_crates(
        &self,
        canonical: &ResolvedPath,
    ) -> Option<&std::collections::HashSet<String>> {
        self.canonical_refs_by_path.get(canonical)
    }
}

fn collect_macro_implicit_refs(module: &Module, out: &mut std::collections::HashSet<ResolvedPath>) {
    for m in module.walk() {
        out.extend(m.macro_implicit_refs.iter().cloned());
    }
}

/// Walk a crate's module tree and collect every canonical path it references,
/// unioning three sources: `use` bindings (declared imports),
/// `Module.references` (regular-code path use), and
/// `Module.macro_implicit_refs` (paths inside the crate's own
/// `macro_rules!` bodies). Macro-body refs belong here too — when crate A's
/// macro body mentions `B::foo`, A genuinely depends on B, so any
/// dep-usage analysis would otherwise wrongly flag B as unused.
///
/// The result populates `Workspace::references_by_crate` once per crate at
/// load time. Note: the per-target-crate set built by
/// [`Workspace::macro_implicit_refs_for`] is a different concept — it's
/// the union of macro-body refs from crates that could plausibly invoke
/// a macro affecting the target crate.
fn collect_module_references(module: &Module, out: &mut std::collections::HashSet<ResolvedPath>) {
    for m in module.walk() {
        out.extend(m.use_bindings.iter().map(|b| b.canonical.clone()));
        out.extend(m.references.iter().cloned());
        out.extend(m.macro_implicit_refs.iter().cloned());
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

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str, krate: &str) -> Item {
        Item {
            name: name.into(),
            kind: ItemKind::Fn,
            visibility: Visibility::Public,
            canonical: ResolvedPath::new([krate.to_string(), name.to_string()]),
            source: None,
            vis_byte_range: None,
        }
    }

    fn module(name: &str, krate: &str, items: Vec<Item>, submodules: Vec<Module>) -> Module {
        Module {
            name: name.into(),
            canonical: ResolvedPath::new([krate.to_string(), name.to_string()]),
            visibility: Visibility::Public,
            items,
            submodules,
            use_bindings: Vec::new(),
            broken_mod_decls: Vec::new(),
            cfg_features: Vec::new(),
            macro_implicit_refs: Vec::new(),
            references: Vec::new(),
            file: None,
        }
    }

    #[test]
    fn resolved_path_display_joins_segments() {
        let p = ResolvedPath::new(["serde", "de", "Deserialize"]);
        assert_eq!(p.display(), "serde::de::Deserialize");
        assert_eq!(p.crate_name(), Some("serde"));
    }

    #[test]
    fn module_items_walks_tree_in_order() {
        let leaf = module("leaf", "demo", vec![item("inner", "demo")], vec![]);
        let root = module(
            "root",
            "demo",
            vec![item("a", "demo"), item("b", "demo")],
            vec![leaf],
        );
        let names: Vec<_> = root.walk_items().map(|(_, i)| i.name.clone()).collect();
        assert_eq!(names, vec!["a", "b", "inner"]);
    }

    #[test]
    fn crate_pub_items_filters_visibility() {
        let mut pub_item = item("visible", "demo");
        let mut priv_item = item("hidden", "demo");
        priv_item.visibility = Visibility::Private;
        pub_item.visibility = Visibility::Public;
        let root = module("root", "demo", vec![pub_item, priv_item], vec![]);
        let lib_target = Target {
            kind: TargetKind::Lib,
            name: "demo".into(),
            src_path: PathBuf::from("src/lib.rs"),
            root,
        };
        let krate = Crate {
            name: "demo".into(),
            version: "0.0.0".into(),
            manifest_dir: PathBuf::new(),
            is_workspace_member: true,
            targets: vec![lib_target],
            orphan_files: Vec::new(),
            declared_features: Vec::new(),
            manifest: crate::manifest::Manifest::empty(),
        };
        let pub_names: Vec<_> = krate.pub_items().map(|i| i.name.clone()).collect();
        assert_eq!(pub_names, vec!["visible"]);
    }
}
