//! Resolved workspace model and the name-resolution pipeline that builds it.
//!
//! The pipeline runs in three tiers, each adding precision:
//!
//! - `use_tree` — Tier 1: per-file `use` and `use ... as ...` tracking.
//! - `module_tree` — Tier 2: cross-file modules (`mod foo;`, `#[path]`).
//! - `re_export` — Tier 2.5: `pub use` chain following.
//!
//! Each tier produces structures that the next consumes; the entry point is
//! [`Workspace::load`], which orchestrates all three. The tier modules are
//! crate-internal; the public surface is the resolved model, re-exported here
//! and at the crate root alongside [`ReExportIndex`](crate::ReExportIndex).
//!
//! The model itself is split across sibling modules: `types` (leaf value types),
//! `model` (the module/target/crate tree), `workspace` (load orchestration), and
//! `queries` (read-only accessors over a loaded workspace).

mod doc_fences;
pub(crate) mod module_tree;
pub(crate) mod re_export;
pub(crate) mod use_tree;

mod model;
mod queries;
#[cfg(test)]
mod tests;
mod types;
mod workspace;

pub use model::{Crate, Module, Target, TargetKind};
pub use types::{
    BrokenModDecl, Error, Item, ItemKind, LoadOptions, LoadWarning, Occurrence, Origin,
    ResolvedPath, Result, SignatureExposure, SourceSpan, Visibility,
};
pub use workspace::Workspace;
