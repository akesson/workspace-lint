//! Layer 2: explicit annotations on macro definitions.
//!
//! Two equivalent forms; both are parsed by walking the file's syntax tree
//! and matching items that appear immediately before a `macro_rules!` or
//! proc-macro definition.
//!
//! ## Macro form (requires `syn-workspace-marker`)
//!
//! ```ignore
//! workspace_syn::expansion_uses!(serde::Serialize, chrono::DateTime);
//! macro_rules! my_macro { /* ... */ }
//! ```
//!
//! ## Comment-directive form (zero deps)
//!
//! ```ignore
//! // workspace-syn: expansion-uses(serde::Serialize, chrono::DateTime)
//! macro_rules! my_macro { /* ... */ }
//! ```
//!
//! Both produce the same set of [`ResolvedPath`](super::super::ResolvedPath)
//! entries. They override (rather than add to) Layer 1 autodetect when both
//! are present on the same macro — annotation is authoritative.
