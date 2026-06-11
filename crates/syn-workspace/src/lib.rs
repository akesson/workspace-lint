//! A resolved workspace model for Rust, built on `syn`.
//!
//! `syn-workspace` fills the gap between per-file `syn` parsing and the full
//! rust-analyzer frontend: it loads a cargo workspace, resolves imports
//! (including `use ... as ...` renames and `pub use` chains), and exposes a
//! typed model that downstream lints can query in sub-second time.
//!
//! Deliberate non-goals: no type inference, no trait solving, no proc-macro
//! execution. The library trades precision for speed; built-in macro-body
//! parsers cover the cases where token-level scanning is insufficient.
//!
//! The whole resolved model is `Send + Sync`, so consumers are free to
//! parallelize their own analyses across crates.
//!
//! # Example
//!
//! ```no_run
//! use syn_workspace::Workspace;
//!
//! let ws = Workspace::load(".")?;
//! for cr in ws.members() {
//!     for item in cr.pub_items() {
//!         println!("{} :: {}", cr.name, item.canonical);
//!     }
//! }
//! # Ok::<(), syn_workspace::Error>(())
//! ```
//!
//! # `toml_edit` re-export
//!
//! [`toml_edit`] is re-exported so callers that inspect dependency
//! entries via [`Manifest::deps`] don't have to add their own dep. This
//! is part of the public API stability contract: a major-version bump
//! in `toml_edit` is a major-version bump in `syn-workspace`. If you
//! only need version strings, prefer `Manifest::get_dep_version` —
//! it doesn't expose `toml_edit` types.

#![forbid(unsafe_code)]

pub mod assertions;
pub mod macros;
pub mod manifest;
pub(crate) mod plugins;
pub mod resolve;
pub mod scip_emit;
mod walk;

/// Re-export `toml_edit` so lint crates can name [`toml_edit::Item`] and
/// related types without adding their own direct dep.
pub use toml_edit;

pub use assertions::{Trigger, UsageAssertion, builtin_assertions};
pub use manifest::{DeclaredDep, DepLocation, DepSection, Manifest, Publish};
pub use resolve::{
    BrokenModDecl, Crate, Error, Item, ItemKind, LoadOptions, LoadWarning, Module, Occurrence,
    Origin, ResolvedPath, Result, SourceSpan, Target, TargetKind, Visibility, Workspace,
    re_export::ReExportIndex,
};
pub use scip_emit::{ScipOccurrence, ScipRole, is_definition_kind};
pub use walk::member_manifests;
