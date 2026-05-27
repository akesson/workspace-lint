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
#[derive(Debug)]
pub enum Error {
    /// Failed to read or parse `Cargo.toml`.
    Manifest(String),
    /// Failed to read or parse a Rust source file.
    Parse { path: PathBuf, message: String },
    /// I/O error during workspace traversal.
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Manifest(msg) => write!(f, "manifest error: {msg}"),
            Self::Parse { path, message } => {
                write!(f, "parse error in {}: {}", path.display(), message)
            }
            Self::Io(err) => write!(f, "I/O error: {err}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
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
}

/// File location of a syntactic element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSpan {
    pub file: PathBuf,
    pub line: u32,
    pub column: u32,
}

/// A `mod foo;` declaration that didn't resolve to a backing file.
///
/// Recorded so downstream lints can flag the mismatch — typically a rename
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
    pub items: Vec<Item>,
    pub submodules: Vec<Module>,
    /// `use` bindings active in this module's scope (renames resolved to
    /// canonical paths). Populated by Tier 1 during the Tier 2 walk; inline
    /// child modules carry their own bindings independently of their parent.
    pub use_bindings: Vec<use_tree::UseBinding>,
    /// `mod foo;` declarations encountered in this module whose target file
    /// couldn't be resolved (and which don't have an inline body). Drives
    /// the module-tree integrity lint.
    pub broken_mod_decls: Vec<BrokenModDecl>,
    /// Feature names referenced via `#[cfg(feature = "...")]` or
    /// `#[cfg_attr(feature = "...", ...)]` on any item declared in this
    /// module (outer attributes only — feature gates inside function
    /// bodies are not extracted here). Deduped, sorted lexicographically.
    pub cfg_features: Vec<String>,
    /// Canonical paths referenced inside `macro_rules!` bodies declared in
    /// this module (Layer 1 autodetect). Conservative: any multi-segment
    /// path appearing in the macro RHS gets resolved through the macro's
    /// defining scope and recorded. Used by downstream lints
    /// (visibility, unused-deps, architecture) to avoid false positives
    /// on items reachable only through workspace-owned macros.
    pub macro_implicit_refs: Vec<ResolvedPath>,
    /// Canonical paths referenced from this module's regular code (function
    /// bodies, type signatures, attribute paths). Distinct from
    /// `use_bindings` (which records what `use` statements bring into scope)
    /// and from `macro_implicit_refs` (which records paths inside
    /// `macro_rules!` bodies). Populated by token-scanning each non-`use`,
    /// non-`macro_rules!` item with use-binding substitution applied to the
    /// leading segment.
    ///
    /// Used by lints that need cross-crate reference graphs without paying
    /// SCIP's cost: unused-deps consults the set of crate names; unused-pub
    /// and visibility consult per-item canonicals.
    pub references: Vec<ResolvedPath>,
    /// File backing this module, if any. `None` for inline `mod foo { ... }`
    /// blocks whose file is the parent.
    pub file: Option<PathBuf>,
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
    /// Crate-root module. For external crates, this is an empty placeholder.
    pub root: Module,
    /// Cargo `[features]` declared in this crate's `Cargo.toml`. Includes
    /// `default` if defined. Activation lists are not retained — the
    /// feature-drift lint only cares about which feature names exist.
    pub declared_features: Vec<String>,
}

impl Crate {
    /// Iterate all items in the crate, recursively.
    pub fn items(&self) -> impl Iterator<Item = &Item> {
        ModuleItems::new(&self.root)
    }

    /// Iterate items whose visibility is reachable outside the crate
    /// (currently: `Public` only — `pub(crate)` is intra-crate).
    pub fn pub_items(&self) -> impl Iterator<Item = &Item> {
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
}

/// The top-level resolved workspace.
#[derive(Debug, Clone)]
pub struct Workspace {
    crates: Vec<Crate>,
    root: PathBuf,
    re_exports: re_export::ReExportIndex,
    /// Union of all macro-implicit references: Layer 1 (autodetect of
    /// `macro_rules!` bodies, eagerly collected from `crates` at load time)
    /// plus Layer 3 (`[[macros.external]]` entries, appended via
    /// `register_external_macro_uses`). Built once; lints borrow it.
    macro_refs: std::collections::HashSet<ResolvedPath>,
    /// Per-crate set of canonical paths referenced from that crate's regular
    /// code (combines `use` bindings + the `Module.references` set). Keyed
    /// by the crate's code name (Cargo-form hyphens replaced with '_'). Built
    /// once at load time so unused-deps / unused-pub / visibility don't
    /// re-walk the tree.
    references_by_crate: std::collections::HashMap<String, std::collections::HashSet<ResolvedPath>>,
}

impl Workspace {
    /// Load and resolve a workspace at the given root directory.
    ///
    /// Builds the full model in one pass: workspace discovery via
    /// `cargo_metadata`, per-crate module-tree assembly (Tier 2) which
    /// threads Tier 1 use-bindings into each [`Module`], and a
    /// workspace-wide `pub use` chain index (Tier 2.5).
    pub fn load(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let crates = crate::walk::load_members(&root)?;
        let re_exports = re_export::ReExportIndex::build(&crates);
        let mut macro_refs = std::collections::HashSet::new();
        for krate in &crates {
            collect_macro_implicit_refs(&krate.root, &mut macro_refs);
        }
        let mut references_by_crate: std::collections::HashMap<
            String,
            std::collections::HashSet<ResolvedPath>,
        > = std::collections::HashMap::new();
        for krate in &crates {
            if !krate.is_workspace_member {
                continue;
            }
            let code_name = krate.name.replace('-', "_");
            let entry = references_by_crate.entry(code_name.clone()).or_default();
            collect_module_references(&krate.root, entry);
            // Cargo dev-deps are used from `tests/`, `benches/`, `examples/`
            // (separate compilation units, not part of the lib/bin module
            // tree). Without scanning these, unused-deps false-positives on
            // every dev-dep used only in integration tests. Attribute their
            // references to the parent crate.
            for aux in ["tests", "benches", "examples"] {
                let aux_dir = krate.manifest_dir.join(aux);
                scan_aux_dir_references(&aux_dir, &code_name, entry);
            }
        }
        Ok(Self {
            crates,
            root,
            re_exports,
            macro_refs,
            references_by_crate,
        })
    }

    /// Register implicit references for macros defined outside the workspace
    /// (Layer 3 — config-driven). Each call appends; deduplication happens
    /// in the underlying [`HashSet`]. Typically invoked by the lint harness
    /// once after [`Workspace::load`], passing entries derived from the
    /// `[[macros.external]]` table in the config file.
    pub fn register_external_macro_uses<I>(&mut self, paths: I)
    where
        I: IntoIterator<Item = ResolvedPath>,
    {
        self.macro_refs.extend(paths);
    }

    /// All workspace member crates plus referenced external crates.
    pub fn crates(&self) -> &[Crate] {
        &self.crates
    }

    /// Just the workspace member crates.
    pub fn members(&self) -> impl Iterator<Item = &Crate> {
        self.crates.iter().filter(|c| c.is_workspace_member)
    }

    /// Workspace root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve a path through any `pub use` re-export chain to its canonical
    /// definition site. Returns the path unchanged if no chain applies.
    pub fn resolve_canonical(&self, path: &ResolvedPath) -> ResolvedPath {
        self.re_exports.canonical(path)
    }

    /// Borrow the underlying re-export index — useful for lints that need to
    /// enumerate all known re-export edges.
    pub fn re_exports(&self) -> &re_export::ReExportIndex {
        &self.re_exports
    }

    /// Union of every `macro_rules!`-body implicit reference across all
    /// workspace members plus any Layer 3 external-macro entries registered
    /// via [`Workspace::register_external_macro_uses`]. Lints consult this
    /// set to avoid flagging items whose only "use" is reachable through a
    /// macro expansion (Layer 1 autodetect — see [`module_tree`] for
    /// extraction).
    ///
    /// The set is built eagerly at [`Workspace::load`] time and stored on
    /// the workspace, so repeated calls across multiple lints are O(1).
    pub fn macro_implicit_refs(&self) -> &std::collections::HashSet<ResolvedPath> {
        &self.macro_refs
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
}

fn collect_macro_implicit_refs(module: &Module, out: &mut std::collections::HashSet<ResolvedPath>) {
    for path in &module.macro_implicit_refs {
        out.insert(path.clone());
    }
    for sub in &module.submodules {
        collect_macro_implicit_refs(sub, out);
    }
}

/// Walk a directory of standalone `.rs` files (cargo's `tests/`, `benches/`,
/// `examples/`), parsing each as a root module and unioning its references
/// into `out`. Subdirectories with a `mod.rs` are walked too (integration
/// tests sometimes split helpers across files). Errors are silently dropped
/// — these directories are conventionally present-or-absent and a parse
/// error on a test file shouldn't crash the resolver for the whole workspace.
fn scan_aux_dir_references(
    aux_dir: &Path,
    crate_name: &str,
    out: &mut std::collections::HashSet<ResolvedPath>,
) {
    let Ok(entries) = std::fs::read_dir(aux_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let canonical = ResolvedPath::new([crate_name.to_string()]);
        if path.extension().is_some_and(|ext| ext == "rs") {
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("aux")
                .to_string();
            if let Ok(module) = module_tree::build_module_from_file(&path, stem, canonical) {
                collect_module_references(&module, out);
            }
        } else if path.is_dir() {
            let nested = path.join("mod.rs");
            if nested.exists() {
                let stem = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("aux_mod")
                    .to_string();
                if let Ok(module) = module_tree::build_module_from_file(&nested, stem, canonical) {
                    collect_module_references(&module, out);
                }
            }
        }
    }
}

/// Walk a crate's module tree and collect every canonical path it references,
/// unioning three sources: `use` bindings (declared imports),
/// `Module.references` (regular-code path use), and
/// `Module.macro_implicit_refs` (paths inside the crate's own
/// `macro_rules!` bodies). Macro-body refs belong here too — when crate A's
/// macro body mentions `B::foo`, A genuinely depends on B, and unused-deps
/// must not flag B as unused.
///
/// The result populates `Workspace::references_by_crate` once per crate at
/// load time. Note: the workspace-wide [`Workspace::macro_implicit_refs`]
/// set is a different concept — it's used as a suppression channel by
/// visibility/unused-pub to flag items reachable through any macro.
fn collect_module_references(module: &Module, out: &mut std::collections::HashSet<ResolvedPath>) {
    for binding in &module.use_bindings {
        out.insert(binding.canonical.clone());
    }
    for path in &module.references {
        out.insert(path.clone());
    }
    for path in &module.macro_implicit_refs {
        out.insert(path.clone());
    }
    for sub in &module.submodules {
        collect_module_references(sub, out);
    }
}

/// Recursive iterator over items in a module tree.
struct ModuleItems<'a> {
    stack: Vec<&'a Module>,
    cursor: Option<(&'a Module, usize)>,
}

impl<'a> ModuleItems<'a> {
    fn new(root: &'a Module) -> Self {
        Self {
            stack: vec![root],
            cursor: None,
        }
    }
}

impl<'a> Iterator for ModuleItems<'a> {
    type Item = &'a Item;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some((module, idx)) = self.cursor.as_mut() {
                if *idx < module.items.len() {
                    let item = &module.items[*idx];
                    *idx += 1;
                    return Some(item);
                }
                self.cursor = None;
            }
            let next = self.stack.pop()?;
            for sub in next.submodules.iter().rev() {
                self.stack.push(sub);
            }
            self.cursor = Some((next, 0));
        }
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
        }
    }

    fn module(name: &str, krate: &str, items: Vec<Item>, submodules: Vec<Module>) -> Module {
        Module {
            name: name.into(),
            canonical: ResolvedPath::new([krate.to_string(), name.to_string()]),
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
        let names: Vec<_> = ModuleItems::new(&root).map(|i| i.name.clone()).collect();
        assert_eq!(names, vec!["a", "b", "inner"]);
    }

    #[test]
    fn crate_pub_items_filters_visibility() {
        let mut pub_item = item("visible", "demo");
        let mut priv_item = item("hidden", "demo");
        priv_item.visibility = Visibility::Private;
        pub_item.visibility = Visibility::Public;
        let root = module("root", "demo", vec![pub_item, priv_item], vec![]);
        let krate = Crate {
            name: "demo".into(),
            version: "0.0.0".into(),
            manifest_dir: PathBuf::new(),
            is_workspace_member: true,
            root,
            declared_features: Vec::new(),
        };
        let pub_names: Vec<_> = krate.pub_items().map(|i| i.name.clone()).collect();
        assert_eq!(pub_names, vec!["visible"]);
    }
}
