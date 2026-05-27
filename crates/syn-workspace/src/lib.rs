//! A resolved workspace model for Rust, built on `syn`.
//!
//! `syn-workspace` fills the gap between per-file `syn` parsing and the full
//! rust-analyzer frontend: it loads a cargo workspace, resolves imports
//! (including `use ... as ...` renames and `pub use` chains), and exposes a
//! typed model that downstream lints can query in sub-second time.
//!
//! Deliberate non-goals: no type inference, no trait solving, no proc-macro
//! execution. The library trades precision for speed; the [`MacroBodyParser`]
//! plugin trait covers the macro-body cases where token-level scanning is
//! insufficient.
//!
//! # Example
//!
//! ```ignore
//! use syn_workspace::Workspace;
//!
//! let ws = Workspace::load(".")?;
//! for cr in ws.crates() {
//!     for item in cr.pub_items() {
//!         println!("{}::{}", cr.name(), item.canonical_path());
//!     }
//! }
//! # Ok::<(), syn_workspace::Error>(())
//! ```

#![forbid(unsafe_code)]

pub mod macros;
pub mod plugins;
pub mod resolve;
mod walk;

pub use plugins::{MacroBodyParser, ResolveContext, builtin_parsers};
pub use resolve::{
    Crate, Error, Item, ItemKind, Module, ResolvedPath, Result, Visibility, Workspace,
};
