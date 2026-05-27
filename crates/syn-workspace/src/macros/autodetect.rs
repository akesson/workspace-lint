//! Layer 1: automatic reference inference for workspace-owned macros.
//!
//! Two passes:
//!
//! - `macro_rules!` definitions: parse the right-hand-side `TokenStream` of
//!   each rule, collect `Ident` tokens in path-like positions (those followed
//!   by `::` or appearing as the first segment of a path), record as
//!   "implicit references" of any call site.
//!
//! - Proc-macro crates (manifests with `proc-macro = true`): scan source for
//!   `quote! { ... }` blocks and `format_ident!` / `Ident::new(...)`
//!   string-literal arguments. Best-effort and documented as imprecise — edge
//!   cases land in `known_false_*` fixtures.
//!
//! Output is merged with Layer 2 (annotations) per macro before downstream
//! lints query the effective reference set.
