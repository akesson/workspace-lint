//! Item lowering and attribute scanning: turn a `syn::Item` into the resolved
//! [`Item`] model ([`item_from_syn`]), pull out a syn item's attributes
//! ([`item_attrs`]), scan `#[cfg(feature = "…")]` predicates, and extract the
//! declaring ident / sibling name used by the occurrence keep-filter.

use std::path::Path;

use super::byte_range;
use super::{Item, ItemKind, ResolvedPath, SourceSpan, Visibility};

/// Outer attributes of a syn item. Returned as a slice so the caller can
/// iterate without copying.
///
/// Mirrored (not reused) by `lints::shipped_source` in the workspace-lint
/// binary; keep the two arm-for-arm identical when a new `syn::Item` variant
/// appears.
pub(crate) fn item_attrs(item: &syn::Item) -> &[syn::Attribute] {
    match item {
        syn::Item::Const(i) => &i.attrs,
        syn::Item::Enum(i) => &i.attrs,
        syn::Item::ExternCrate(i) => &i.attrs,
        syn::Item::Fn(i) => &i.attrs,
        syn::Item::ForeignMod(i) => &i.attrs,
        syn::Item::Impl(i) => &i.attrs,
        syn::Item::Macro(i) => &i.attrs,
        syn::Item::Mod(i) => &i.attrs,
        syn::Item::Static(i) => &i.attrs,
        syn::Item::Struct(i) => &i.attrs,
        syn::Item::Trait(i) => &i.attrs,
        syn::Item::TraitAlias(i) => &i.attrs,
        syn::Item::Type(i) => &i.attrs,
        syn::Item::Union(i) => &i.attrs,
        syn::Item::Use(i) => &i.attrs,
        syn::Item::Verbatim(_) => &[],
        _ => &[],
    }
}

/// Scan an attribute for `feature = "name"` predicates inside `cfg(...)` or
/// `cfg_attr(<cfg>, ...)`. Predicates can be nested under `any(...)`,
/// `all(...)`, and `not(...)`; we recurse through the meta-list tree.
pub(super) fn extract_cfg_feature_names(
    attr: &syn::Attribute,
    out: &mut std::collections::BTreeSet<String>,
) {
    let ident = match attr.path().get_ident() {
        Some(i) => i.to_string(),
        None => return,
    };
    if ident != "cfg" && ident != "cfg_attr" {
        return;
    }
    // Parse the inner meta. cfg(...) and cfg_attr(<cfg>, ...) both start
    // with a Meta::List whose nested predicate-tree we scan.
    if let syn::Meta::List(list) = &attr.meta {
        scan_cfg_tokens(list.tokens.clone(), out);
    }
}

fn scan_cfg_tokens(tokens: proc_macro2::TokenStream, out: &mut std::collections::BTreeSet<String>) {
    let iter: Vec<proc_macro2::TokenTree> = tokens.into_iter().collect();
    let mut i = 0;
    while i < iter.len() {
        if let proc_macro2::TokenTree::Ident(id) = &iter[i] {
            let name = id.to_string();
            if name == "feature"
                && let Some(proc_macro2::TokenTree::Punct(p)) = iter.get(i + 1)
                && p.as_char() == '='
                && let Some(proc_macro2::TokenTree::Literal(lit)) = iter.get(i + 2)
            {
                let s = lit.to_string();
                let trimmed = s.trim_matches('"');
                if !trimmed.is_empty() {
                    out.insert(trimmed.to_string());
                }
                i += 3;
                continue;
            }
        }
        if let proc_macro2::TokenTree::Group(g) = &iter[i] {
            scan_cfg_tokens(g.stream(), out);
        }
        i += 1;
    }
}

/// The ident that *declares* an item, when the item introduces a single name —
/// the `fn`/`struct`/`enum`/… name. Returns a borrow so callers can read its
/// span (the declaration site) as well as its text. The single source of truth
/// for both [`sibling_name`] (the module's lexical name set) and the
/// declaration-site skip in [`extract_code_paths`].
pub(super) fn decl_ident(item: &syn::Item) -> Option<&syn::Ident> {
    match item {
        syn::Item::Fn(i) => Some(&i.sig.ident),
        syn::Item::Struct(i) => Some(&i.ident),
        syn::Item::Enum(i) => Some(&i.ident),
        syn::Item::Union(i) => Some(&i.ident),
        syn::Item::Trait(i) => Some(&i.ident),
        syn::Item::Type(i) => Some(&i.ident),
        syn::Item::Const(i) => Some(&i.ident),
        syn::Item::Static(i) => Some(&i.ident),
        syn::Item::Mod(i) => Some(&i.ident),
        // A `macro_rules!` definition introduces a name in the *macro*
        // namespace only. A path-position reference like `log::debug` — or a
        // bare type/value ident — resolves in the type/value/module namespace,
        // where the macro name does not participate, so a macro must NOT be a
        // sibling that shadows an external-crate reference of the same name
        // (e.g. memchr's `macro_rules! log` vs. the `log` crate, where
        // `log::debug!` inside another macro is the only use of `log`). A name
        // that is *also* a module/type/etc. still enters via that item's arm.
        syn::Item::Macro(_) => None,
        _ => None,
    }
}

/// Names declared at a module's lexical scope — function/struct/enum/etc.
/// idents plus child module names. Used to distinguish "crate-local sibling"
/// from "external crate" at the leading segment of a `use` path.
pub(super) fn sibling_name(item: &syn::Item) -> Option<String> {
    decl_ident(item).map(|ident| ident.to_string())
}

/// Lower a `syn::Item` to the resolved [`Item`] model: its name, [`ItemKind`],
/// [`Visibility`], canonical path under `parent_canonical`, declaration
/// [`SourceSpan`], and the byte range of its `vis` keyword (for visibility
/// rewriters). Returns `None` for items that introduce no referenceable name.
pub(super) fn item_from_syn(
    item: &syn::Item,
    parent_canonical: &ResolvedPath,
    file: &Path,
) -> Option<Item> {
    // Full span of the item, used by callers that need to rewrite the
    // item structurally (e.g. visibility tighteners, dead-code removers).
    let full_span = match item {
        syn::Item::Fn(i) => Some(syn::spanned::Spanned::span(i)),
        syn::Item::Struct(i) => Some(syn::spanned::Spanned::span(i)),
        syn::Item::Enum(i) => Some(syn::spanned::Spanned::span(i)),
        syn::Item::Union(i) => Some(syn::spanned::Spanned::span(i)),
        syn::Item::Trait(i) => Some(syn::spanned::Spanned::span(i)),
        syn::Item::Type(i) => Some(syn::spanned::Spanned::span(i)),
        syn::Item::Const(i) => Some(syn::spanned::Spanned::span(i)),
        syn::Item::Static(i) => Some(syn::spanned::Spanned::span(i)),
        syn::Item::Mod(i) => Some(syn::spanned::Spanned::span(i)),
        syn::Item::Macro(i) => Some(syn::spanned::Spanned::span(i)),
        _ => None,
    };
    let item_byte_range = full_span.and_then(byte_range);

    let (name, kind, vis, line) = match item {
        syn::Item::Fn(i) => (
            i.sig.ident.to_string(),
            ItemKind::Fn,
            &i.vis,
            i.sig.ident.span().start().line,
        ),
        syn::Item::Struct(i) => (
            i.ident.to_string(),
            ItemKind::Struct,
            &i.vis,
            i.ident.span().start().line,
        ),
        syn::Item::Enum(i) => (
            i.ident.to_string(),
            ItemKind::Enum,
            &i.vis,
            i.ident.span().start().line,
        ),
        syn::Item::Union(i) => (
            i.ident.to_string(),
            ItemKind::Union,
            &i.vis,
            i.ident.span().start().line,
        ),
        syn::Item::Trait(i) => (
            i.ident.to_string(),
            ItemKind::Trait,
            &i.vis,
            i.ident.span().start().line,
        ),
        syn::Item::Type(i) => (
            i.ident.to_string(),
            ItemKind::TypeAlias,
            &i.vis,
            i.ident.span().start().line,
        ),
        syn::Item::Const(i) => (
            i.ident.to_string(),
            ItemKind::Const,
            &i.vis,
            i.ident.span().start().line,
        ),
        syn::Item::Static(i) => (
            i.ident.to_string(),
            ItemKind::Static,
            &i.vis,
            i.ident.span().start().line,
        ),
        syn::Item::Mod(i) => (
            i.ident.to_string(),
            ItemKind::Module,
            &i.vis,
            i.ident.span().start().line,
        ),
        syn::Item::Macro(i) => {
            // `macro_rules!` definitions; only emit if named.
            let name = i.ident.as_ref()?.to_string();
            // `macro_rules!` has no `pub` token at the syn level — exports go
            // via `#[macro_export]` attribute. Treat exported macros as
            // Public, others as Private.
            let exported = i.attrs.iter().any(|a| a.path().is_ident("macro_export"));
            let vis = if exported {
                Visibility::Public
            } else {
                Visibility::Private
            };
            let mut canonical = parent_canonical.segments().to_vec();
            canonical.push(name.clone());
            return Some(Item {
                name,
                kind: ItemKind::Macro,
                visibility: vis,
                canonical: ResolvedPath::new(canonical),
                source: Some(SourceSpan {
                    file: file.to_path_buf(),
                    line: i.ident.as_ref().unwrap().span().start().line as u32,
                    column: 1,
                    byte_range: item_byte_range.clone(),
                }),
                // Macros don't expose a `pub` token; visibility is governed
                // by `#[macro_export]` instead, so structural-fix consumers
                // have nothing to rewrite here.
                vis_byte_range: None,
            });
        }
        _ => return None,
    };

    // For public items, capture the byte range of the `pub` keyword itself.
    // Structural-fix consumers narrow `pub` to `pub(crate)` (etc.) by
    // overwriting that range — no scanning past preceding doc comments
    // or attributes required.
    let vis_byte_range = match vis {
        syn::Visibility::Public(token) => byte_range(token.span),
        _ => None,
    };
    let mut canonical = parent_canonical.segments().to_vec();
    canonical.push(name.clone());
    Some(Item {
        name,
        kind,
        visibility: Visibility::from_syn(vis),
        canonical: ResolvedPath::new(canonical),
        source: Some(SourceSpan {
            file: file.to_path_buf(),
            line: line as u32,
            column: 1,
            byte_range: item_byte_range,
        }),
        vis_byte_range,
    })
}
