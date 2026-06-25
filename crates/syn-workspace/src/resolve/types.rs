//! Leaf value types of the resolved model: the error/warning/option types, the
//! canonical [`ResolvedPath`], the [`Visibility`]/[`ItemKind`] enums, the
//! [`Item`]/[`SourceSpan`]/[`BrokenModDecl`] records, and the reference
//! [`Origin`]/[`Occurrence`] pair. The tree model that holds them lives in
//! `model`; the workspace that owns the tree lives in `workspace`.

use std::path::PathBuf;

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

/// Configuration for [`Workspace::load_with_options`](crate::Workspace::load_with_options).
///
/// All fields have sensible defaults; reach for this struct only when
/// you need to override one — most callers can stick with
/// [`Workspace::load`](crate::Workspace::load).
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

/// A type path that appears in the *public signature surface* of an item — a
/// `pub fn` parameter/return type, a `pub` field type, a trait-impl
/// associated-type value, a type-alias RHS, etc., including types nested inside
/// generic arguments (`Vec<Foo>`, `Result<Foo>`). Collected per module by the
/// signature walk and aggregated into
/// [`Workspace::exposed_in_public_signature`](crate::Workspace::exposed_in_public_signature).
///
/// `enclosing_vis` is the visibility of the item whose signature mentions the
/// type. It bounds how far the referenced type's own visibility may be narrowed:
/// Rust forbids a more-visible item from exposing a less-visible type — E0446
/// (hard error) for a trait-impl associated type, the `private_interfaces` lint
/// for fn signatures and fields. `unused-pub` reads this to avoid suggesting a
/// `pub(crate)` tighten that would not compile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureExposure {
    /// Canonical path of the type mentioned in the signature.
    pub canonical: ResolvedPath,
    /// Visibility of the item whose signature mentions it.
    pub enclosing_vis: Visibility,
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

/// Where a reference [`Occurrence`] came from. Splits the occurrence stream
/// into the two reference channels consumers care about: `Macro` bodies vs.
/// everything else (regular code, `use` globs, `extern crate`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Origin {
    /// A path referenced from regular (non-macro) code.
    Code,
    /// The prefix of a glob import (`use foo::bar::*`).
    GlobUse,
    /// An `extern crate foo;` declaration.
    ExternCrate,
    /// A path inside a macro body (`macro_rules!`, `expansion_uses!`, or a
    /// plugin-lowered macro such as `rsx!`).
    Macro,
    /// A bare framework-component name captured but left unresolved by the
    /// central resolver — a Phase B resolver plugin binds it against the
    /// workspace. Two capture sources today, both Dioxus: the `rsx!` lowerer
    /// (a `Foo {}` invocation) and the `#[derive(Routable)]` enum walk (a route
    /// variant ident or `#[layout(Foo)]` component). The dioxus plugin's
    /// `global_facts` hook binds either to a same-crate `pub fn` of that name.
    Component,
    /// A bare single-ident macro *invocation* (`foo!(…)` / `foo![…]` / `foo!{…}`)
    /// in regular code. Captured here but left unresolved by the central resolver
    /// (a bare macro name carries no path scope); the core Phase B `MacroCallPass`
    /// binds it to a same-crate `macro_rules!` definition of that name.
    /// Multi-segment macro paths (`m::foo!`) are ordinary [`Origin::Code`] runs,
    /// not this.
    MacroCall,
    /// A bare single ident in a module that has at least one glob import
    /// (`use m::*;`), matching neither a `use` binding nor a same-module
    /// sibling — potentially a name the glob brought into scope. Left
    /// unresolved by the central resolver (a glob's contents aren't known
    /// per-file); the core Phase B `GlobImportPass` binds it against the
    /// glob target's items when the target is a workspace module. Most
    /// captures are local variables and never bind — excluded from the SCIP
    /// projection like [`Origin::Component`] / [`Origin::MacroCall`].
    GlobCandidate,
}

/// A single reference occurrence in a module — the resolver's primary
/// reference surface. Carries the raw path segments as written (Phase A
/// extraction), the canonicalized `path` (Phase B resolution; `None` when the
/// path couldn't be resolved, e.g. an unmatched single ident), the source
/// `span` of the reference site, and its [`Origin`]. The two path forms give
/// two diff points for the differential oracle: raw segments localize
/// extraction bugs, resolved paths localize resolution bugs.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Occurrence {
    /// Raw path segments as written (no peeling/substitution). For `GlobUse` /
    /// `ExternCrate` origins these are already the resolved segments.
    pub segments: Vec<String>,
    /// Canonical resolved path, or `None` if the occurrence didn't resolve.
    pub path: Option<ResolvedPath>,
    /// Source span of the reference site.
    pub span: Option<SourceSpan>,
    /// Which extraction channel produced this occurrence.
    pub origin: Origin,
}
