//! Layer 1: automatic reference inference for workspace-owned macros.
//!
//! Token-level scanning of macro bodies for path-like `Ident :: Ident
//! (:: Ident)*` sequences. Emits each as a raw `Macro`-origin `Occurrence`
//! (spanned); canonicalization happens centrally in `resolve_occurrence`.
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

use std::path::Path;

use crate::resolve::module_tree::{consume_path_run, span_to_source_span};
use crate::resolve::{Occurrence, Origin};

/// Scan a macro body token-stream for path-like sequences
/// (`Ident :: Ident (:: Ident)*`) and emit each as a raw `Origin::Macro`
/// [`Occurrence`] (segments + span) into `out`. Resolution happens centrally in
/// `resolve_occurrence`.
pub(crate) fn extract_macro_paths(
    tokens: proc_macro2::TokenStream,
    file: &Path,
    out: &mut Vec<Occurrence>,
) {
    let stream: Vec<proc_macro2::TokenTree> = tokens.into_iter().collect();
    let mut i = 0;
    while i < stream.len() {
        if let proc_macro2::TokenTree::Ident(first) = &stream[i] {
            let (segments, j) = consume_path_run(&stream, i);
            // Candidate selection only (multi-segment runs) — resolution is
            // central in `resolve_occurrence`.
            if segments.len() >= 2 {
                out.push(Occurrence {
                    segments,
                    path: None,
                    span: Some(span_to_source_span(file, first.span())),
                    origin: Origin::Macro,
                });
            }
            i = j;
            continue;
        }
        if let proc_macro2::TokenTree::Group(group) = &stream[i] {
            extract_macro_paths(group.stream(), file, out);
        }
        i += 1;
    }
}
