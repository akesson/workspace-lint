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
}

/// The top-level resolved workspace.
#[derive(Debug, Clone)]
pub struct Workspace {
    crates: Vec<Crate>,
    root: PathBuf,
    re_exports: re_export::ReExportIndex,
    /// Layer 3: external-macro implicit references contributed by config
    /// rather than scanned from source. Populated by callers after `load`.
    extra_macro_refs: std::collections::HashSet<ResolvedPath>,
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
        Ok(Self {
            crates,
            root,
            re_exports,
            extra_macro_refs: std::collections::HashSet::new(),
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
        self.extra_macro_refs.extend(paths);
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
    /// workspace members. Lints consult this set to avoid flagging items
    /// whose only "use" is reachable through a workspace-owned macro
    /// expansion (Layer 1 autodetect — see [`module_tree`] for extraction).
    pub fn macro_implicit_refs(&self) -> std::collections::HashSet<ResolvedPath> {
        let mut out = std::collections::HashSet::new();
        for krate in &self.crates {
            collect_macro_implicit_refs(&krate.root, &mut out);
        }
        for path in &self.extra_macro_refs {
            out.insert(path.clone());
        }
        out
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
