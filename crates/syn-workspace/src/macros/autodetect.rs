//! Layer 1: automatic reference inference for workspace-owned macros.
//!
//! Token-level scanning of macro bodies for path-like `Ident :: Ident
//! (:: Ident)*` sequences. Resolves each through the macro's defining
//! scope (using `crate::resolve::module_tree::resolve_macro_path`) and
//! records the canonical path in the caller's output set.
//!
//! **Recall vs. precision.** Any multi-segment path shape *anywhere*
//! inside *any* `macro_rules!` body ends up in the implicit-refs set.
//! That set then feeds [`crate::Workspace::macro_implicit_refs_for`],
//! which lets consumers decide whether a given canonical "could plausibly
//! be reached via a macro." A match-arm pattern like `Foo::Bar` or a
//! template literal `quote! { Type::method }` therefore contributes
//! `Foo::Bar` / `Type::method` to that set, regardless of whether the
//! macro's call site is anywhere near those items.
//!
//! This errs toward over-inclusion. Consumers that use the set as a
//! suppression channel (e.g. "don't flag this public item as unused if
//! any macro could be reaching it") will miss some genuine findings but
//! avoid false positives — the assumption is that wrongly flagging an
//! item that *is* referenced via macro expansion is worse than missing
//! a few. Consumers with different tolerances can ignore the set or
//! re-narrow it themselves.
//!
//! Single identifiers (parameter names, keywords, etc.) are dropped.
//! Token groups (`{}`, `()`, `[]`) are recursed into so paths inside
//! nested groups are still seen. String literals are tokenized as
//! `Literal` and so do not produce false positives.

use std::collections::HashSet;
use std::path::Path;

use crate::resolve::ResolvedPath;
use crate::resolve::module_tree::{Occurrence, Origin, resolve_macro_path, span_to_source_span};
use crate::resolve::use_tree;

/// Scan a `macro_rules!` body token-stream for path-like sequences
/// (`Ident :: Ident (:: Ident)*`) and resolve each through the macro's
/// defining scope. Records the resolved path in `out`.
pub(crate) fn extract_macro_paths(
    tokens: proc_macro2::TokenStream,
    scope: &use_tree::Scope,
    siblings: &HashSet<String>,
    parent_canonical: &ResolvedPath,
    file: &Path,
    out: &mut Vec<Occurrence>,
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
            if segments.len() >= 2 {
                let span = span_to_source_span(file, first.span());
                if let Some(resolved) =
                    resolve_macro_path(segments, scope, siblings, parent_canonical)
                {
                    out.push(Occurrence {
                        path: resolved,
                        span: Some(span),
                        origin: Origin::Macro,
                    });
                }
            }
            i = j;
            continue;
        }
        if let proc_macro2::TokenTree::Group(group) = &stream[i] {
            extract_macro_paths(group.stream(), scope, siblings, parent_canonical, file, out);
        }
        i += 1;
    }
}
