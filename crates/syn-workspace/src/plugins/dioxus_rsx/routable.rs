//! Capture: `#[derive(Routable)]` route enums → component references.
//!
//! Dioxus's router derives an enum where each `#[route(...)]` variant maps to a
//! `pub fn` component and each `#[layout(Comp)]` attribute names a wrapper
//! component:
//!
//! ```ignore
//! #[derive(Routable, Clone, PartialEq)]
//! enum Route {
//!     #[layout(NavBar)]   // -> pub fn NavBar
//!     #[route("/")]
//!     DogView,            // variant ident -> pub fn DogView
//!     #[route("/fav")]
//!     Favorites,          // variant ident -> pub fn Favorites
//! }
//! ```
//!
//! Those component functions are referenced *only* through the code the derive
//! macro generates — never by a bare `rsx!` call or a `use` — so the resolver's
//! token/AST scans never see them and `unused-pub` false-positives them as dead.
//!
//! This is the Phase A *capture* half: it emits each referenced component name
//! as a bare-ident [`Origin::Component`] occurrence. [`super::DioxusPlugin`]'s
//! `global_facts` hook (Phase B) then binds each to the matching same-crate
//! `pub fn` — identical handling to a bare `rsx!` component, so no new plugin
//! hook is needed.

use std::path::Path;

use syn::punctuated::Punctuated;
use syn::{Expr, Token};

use crate::resolve::module_tree::span_to_source_span;
use crate::resolve::{Occurrence, Origin};

/// Emit a bare-ident [`Origin::Component`] occurrence for every component named
/// by a `#[derive(Routable)]` enum's `#[route(...)]` / `#[layout(...)]`
/// attributes. Returns an empty vec for any enum that doesn't derive `Routable`.
///
/// Binding rule (matches the dioxus router-macro): a `#[route("/path")]` variant
/// binds the **variant ident**; an explicit `#[route("/path", Comp)]` second arg
/// overrides it; each `#[layout(Comp)]` binds `Comp`. `#[nest]`, `#[redirect]`,
/// `#[child]`, and the `#[end_*]` markers name no leaf component and are ignored.
pub(crate) fn route_component_occurrences(
    item_enum: &syn::ItemEnum,
    file: &Path,
) -> Vec<Occurrence> {
    if !derives_routable(&item_enum.attrs) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for variant in &item_enum.variants {
        let mut route_present = false;
        let mut route_override: Option<syn::Ident> = None;
        for attr in &variant.attrs {
            if attr.path().is_ident("route") {
                route_present = true;
                route_override = route_component_override(attr);
            } else if attr.path().is_ident("layout")
                && let Some(ident) = layout_component(attr)
            {
                push_component(&mut out, &ident, file);
            }
        }
        // Explicit override replaces the variant-ident default; otherwise a
        // routed variant binds its own ident (the macro's
        // `comp_name.unwrap_or(variant.ident)`).
        if let Some(ident) = route_override {
            push_component(&mut out, &ident, file);
        } else if route_present {
            push_component(&mut out, &variant.ident, file);
        }
    }
    out
}

/// True iff the `#[derive(...)]` list contains a path whose final segment is
/// `Routable` (covers both `Routable` and a qualified `dioxus_router::Routable`).
fn derives_routable(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        if !attr.path().is_ident("derive") {
            return false;
        }
        let mut found = false;
        // `parse_nested_meta` visits each path in the derive list; a derive list
        // is always valid nested meta, so the parse can't legitimately fail.
        let _ = attr.parse_nested_meta(|meta| {
            if meta
                .path
                .segments
                .last()
                .is_some_and(|s| s.ident == "Routable")
            {
                found = true;
            }
            Ok(())
        });
        found
    })
}

/// The optional explicit component path in `#[route("/path", Comp)]` — the
/// second positional argument. `None` for the common `#[route("/path")]` form.
fn route_component_override(attr: &syn::Attribute) -> Option<syn::Ident> {
    let args: Punctuated<Expr, Token![,]> =
        attr.parse_args_with(Punctuated::parse_terminated).ok()?;
    match args.iter().nth(1)? {
        Expr::Path(expr_path) => expr_path.path.segments.last().map(|s| s.ident.clone()),
        _ => None,
    }
}

/// The component path in `#[layout(Comp)]` / `#[layout(Comp, ..props)]` — the
/// leading path; trailing prop tokens (if any) are consumed and discarded.
fn layout_component(attr: &syn::Attribute) -> Option<syn::Ident> {
    attr.parse_args_with(|input: syn::parse::ParseStream| {
        let path: syn::Path = input.parse()?;
        // Discard any trailing `, ..props` so the whole arg stream parses.
        input.parse::<proc_macro2::TokenStream>()?;
        Ok(path)
    })
    .ok()?
    .segments
    .last()
    .map(|s| s.ident.clone())
}

fn push_component(out: &mut Vec<Occurrence>, ident: &syn::Ident, file: &Path) {
    out.push(Occurrence {
        segments: vec![ident.to_string()],
        path: None,
        span: Some(span_to_source_span(file, ident.span())),
        origin: Origin::Component,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use quote::quote;

    /// Capture the route/layout component names from an enum literal, sorted for
    /// stable comparison.
    fn names(item: proc_macro2::TokenStream) -> Vec<String> {
        let item_enum: syn::ItemEnum = syn::parse2(item).expect("valid enum");
        let mut got: Vec<String> = route_component_occurrences(&item_enum, Path::new("lib.rs"))
            .into_iter()
            .map(|o| o.segments.join("::"))
            .collect();
        got.sort();
        got
    }

    #[test]
    fn routed_variant_binds_its_ident() {
        assert_eq!(
            names(quote! {
                #[derive(Routable, Clone)]
                enum Route {
                    #[route("/")]
                    Home,
                }
            }),
            vec!["Home"]
        );
    }

    #[test]
    fn layout_binds_explicit_component_alongside_variant() {
        assert_eq!(
            names(quote! {
                #[derive(Routable, Clone)]
                enum Route {
                    #[layout(NavBar)]
                    #[route("/")]
                    Home,
                }
            }),
            vec!["Home", "NavBar"]
        );
    }

    #[test]
    fn explicit_route_component_overrides_variant_ident() {
        // The 2nd `#[route]` arg names the component; the variant ident `Index`
        // is NOT bound.
        assert_eq!(
            names(quote! {
                #[derive(Routable, Clone)]
                enum Route {
                    #[route("/", IndexComp)]
                    Index,
                }
            }),
            vec!["IndexComp"]
        );
    }

    #[test]
    fn enum_without_routable_derive_binds_nothing() {
        assert!(
            names(quote! {
                #[derive(Clone, PartialEq)]
                enum Route {
                    #[route("/")]
                    Home,
                }
            })
            .is_empty()
        );
    }

    #[test]
    fn variant_without_route_or_layout_binds_nothing() {
        // A `#[child]`/marker-only variant names no leaf component.
        assert!(
            names(quote! {
                #[derive(Routable, Clone)]
                enum Route {
                    #[child]
                    Nested(Other),
                }
            })
            .is_empty()
        );
    }

    #[test]
    fn occurrences_are_bare_unresolved_component_origin() {
        let item_enum: syn::ItemEnum = syn::parse2(quote! {
            #[derive(Routable, Clone)]
            enum Route {
                #[layout(NavBar)]
                #[route("/")]
                Home,
            }
        })
        .unwrap();
        let occs = route_component_occurrences(&item_enum, Path::new("lib.rs"));
        assert_eq!(occs.len(), 2);
        for occ in &occs {
            assert_eq!(occ.origin, Origin::Component);
            assert_eq!(occ.segments.len(), 1, "bare single-ident: {occ:?}");
            assert!(occ.path.is_none(), "left unresolved for Phase B: {occ:?}");
            assert!(occ.span.is_some());
        }
    }
}
