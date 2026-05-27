//! Tier 2.5: `pub use` re-export chain following.
//!
//! Once Tiers 1 and 2 have produced per-file scopes and module trees, this
//! tier builds a graph of `pub use` edges and computes the canonical
//! definition for every re-exported name.
//!
//! Example chain:
//!
//! ```ignore
//! // in crate `data-models`
//! pub mod internal { pub struct User; }
//! pub use internal::User;            // edge: data_models::User -> data_models::internal::User
//!
//! // in crate `data-api`
//! pub use data_models::User;          // edge: data_api::User -> data_models::User
//! ```
//!
//! `Workspace::resolve(&path)` chases edges until it reaches a non-`pub use`
//! item, returning the canonical [`ResolvedPath`](super::ResolvedPath).
//!
//! Cycles (`pub use self::X` etc.) are detected and reported via the
//! `module-tree` lint's `pub_use_self_cycle` case.
