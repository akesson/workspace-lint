//! Layer 1: automatic reference inference for workspace-owned macros.
//!
//! Token-level scanning of macro bodies for path-like `Ident :: Ident
//! (:: Ident)*` sequences. Resolves each through the macro's defining
//! scope (using `crate::resolve::module_tree::resolve_macro_path`) and
//! records the canonical path in the caller's output set.
//!
//! **Suppression bias — important to understand.** Any multi-segment path
//! shape *anywhere* inside *any* `macro_rules!` body in the workspace ends
//! up in the implicit-refs set. That set is union'd across every member
//! crate (see `crate::Workspace::macro_implicit_refs`) and lints like
//! `visibility` and `unused-pub` skip items whose canonical appears in it.
//!
//! In other words: a match-arm pattern like `Foo::Bar` or a hand-written
//! template literal `quote! { Type::method }` will silence visibility
//! findings for `Foo::Bar` / `Type::method` workspace-wide, regardless of
//! where the macro lives or whether the call site is anywhere near the
//! flagged item. This errs strongly toward false-negatives (missed
//! findings) over false-positives (incorrect findings) — the assumption is
//! that flagging a public item that is, in fact, referenced by *some*
//! macro expansion is worse than missing a few unused items.
//!
//! Single identifiers (parameter names, keywords, etc.) are dropped.
//! Token groups (`{}`, `()`, `[]`) are recursed into so paths inside
//! nested groups are still seen. String literals are tokenized as
//! `Literal` and so do not produce false positives.

use std::collections::{BTreeSet, HashSet};

use crate::resolve::ResolvedPath;
use crate::resolve::module_tree::resolve_macro_path;
use crate::resolve::use_tree;

/// Scan a `macro_rules!` body token-stream for path-like sequences
/// (`Ident :: Ident (:: Ident)*`) and resolve each through the macro's
/// defining scope. Records the resolved path in `out`.
pub(crate) fn extract_macro_paths(
    tokens: proc_macro2::TokenStream,
    scope: &use_tree::Scope,
    siblings: &HashSet<String>,
    parent_canonical: &ResolvedPath,
    out: &mut BTreeSet<ResolvedPath>,
) {
    let stream: Vec<proc_macro2::TokenTree> = tokens.into_iter().collect();
    let mut i = 0;
    while i < stream.len() {
        if let proc_macro2::TokenTree::Ident(first) = &stream[i] {
            let mut segments = vec![first.to_string()];
            let mut j = i + 1;
            while let (Some(p1), Some(p2), Some(next)) =
                (stream.get(j), stream.get(j + 1), stream.get(j + 2))
            {
                let (proc_macro2::TokenTree::Punct(a), proc_macro2::TokenTree::Punct(b)) = (p1, p2)
                else {
                    break;
                };
                if a.as_char() != ':' || b.as_char() != ':' {
                    break;
                }
                let proc_macro2::TokenTree::Ident(next) = next else {
                    break;
                };
                segments.push(next.to_string());
                j += 3;
            }
            if segments.len() >= 2
                && let Some(resolved) =
                    resolve_macro_path(segments, scope, siblings, parent_canonical)
            {
                out.insert(resolved);
            }
            i = j;
            continue;
        }
        if let proc_macro2::TokenTree::Group(group) = &stream[i] {
            extract_macro_paths(group.stream(), scope, siblings, parent_canonical, out);
        }
        i += 1;
    }
}
