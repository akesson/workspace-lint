//! Plugin: builder-macro return-type exposures (`typed_builder` / `derive_builder`).
//!
//! `typed_builder` and `derive_builder` both generate a public `build()` method whose
//! return type can name a user-declared type that appears in no *source* signature —
//! only inside the struct's `#[builder(…)]` attribute. Two forms promote such a type
//! into the generated signature:
//!
//! - typed_builder `#[builder(build_method(into = T))]` ⇒ `pub fn build(self) -> T`
//! - derive_builder `#[builder(build_fn(error = "T"))]` ⇒ `pub fn build(&self) -> Result<Self, T>`
//!
//! This plugin records every type named in `<Type>` as a `Public`
//! [`SignatureExposure`] (via [`record_exposed_type`], so nested generics like
//! `Result<Foo, FooErr>` are covered), so `unused-pub` never narrows it below `pub`
//! (E0446 / `private_interfaces`). Keyed on the attribute *shape* alone — not gated on
//! the `#[derive(…)]` — because recording is FP-safe (a [`Fact::Exposure`] can only
//! suppress a tighten, never create a finding) and these shapes are near-unique
//! fingerprints of the two macros. `#[cfg_attr(…, builder(…))]` wrappers aren't
//! unwrapped, so those rare forms are (FP-safely) missed.

use syn::spanned::Spanned;

use super::{Fact, LocalFactCtx, Provenance, ResolverPlugin};
use crate::resolve::module_tree::items::item_attrs;
use crate::resolve::module_tree::signature::record_exposed_type;
use crate::resolve::{SignatureExposure, Visibility};

/// Recognizes the two builder-macro attributes that promote a user type into a
/// generated public `build()` signature, recording each as a `Public` exposure.
pub(crate) struct BuilderAttrPlugin;

impl ResolverPlugin for BuilderAttrPlugin {
    fn local_facts(&self, item: &syn::Item, cx: &LocalFactCtx) -> Vec<Fact> {
        let mut out = Vec::new();
        for attr in item_attrs(item) {
            if attr.path().is_ident("builder") {
                record_typed_builder_into(attr, cx, &mut out);
                record_derive_builder_error(attr, cx, &mut out);
            }
        }
        out
    }
}

/// `#[builder(build_method(into = <Type>))]` → expose `<Type>` at `Public`.
fn record_typed_builder_into(attr: &syn::Attribute, cx: &LocalFactCtx, out: &mut Vec<Fact>) {
    let mut exps: Vec<SignatureExposure> = Vec::new();
    let _ = attr.parse_nested_meta(|outer| {
        if !outer.path.is_ident("build_method") {
            return skip_meta_entry(&outer);
        }
        outer.parse_nested_meta(|inner| {
            if !inner.path.is_ident("into") {
                return skip_meta_entry(&inner);
            }
            // A bare `into` (no `= <type>`) yields a generic builder with no concrete
            // type to record; `value()` errors and we move on.
            if let Ok(value) = inner.value()
                && let Ok(ty) = value.parse::<syn::Type>()
            {
                record_exposed_type(&ty, Visibility::Public, cx, &mut exps);
            }
            Ok(())
        })
    });
    push_exposures(exps, attr, cx, "typed_builder", "build_method.into", out);
}

/// `#[builder(build_fn(error = "<Type>"))]` → parse the string as a type and expose it
/// at `Public`. (The `error(…)` list form is FP-safely skipped.)
fn record_derive_builder_error(attr: &syn::Attribute, cx: &LocalFactCtx, out: &mut Vec<Fact>) {
    let mut exps: Vec<SignatureExposure> = Vec::new();
    let _ = attr.parse_nested_meta(|outer| {
        if !outer.path.is_ident("build_fn") {
            return skip_meta_entry(&outer);
        }
        outer.parse_nested_meta(|inner| {
            if !inner.path.is_ident("error") {
                return skip_meta_entry(&inner);
            }
            let Ok(value) = inner.value() else {
                return skip_meta_entry(&inner);
            };
            if let Ok(lit) = value.parse::<syn::LitStr>()
                && let Ok(ty) = syn::parse_str::<syn::Type>(&lit.value())
            {
                record_exposed_type(&ty, Visibility::Public, cx, &mut exps);
            }
            Ok(())
        })
    });
    push_exposures(exps, attr, cx, "derive_builder", "build_fn.error", out);
}

/// Wrap resolved exposures as [`Fact::Exposure`], tagging each with the builder
/// plugin's [`Provenance`] anchored at the `#[builder]` attribute's span.
fn push_exposures(
    exps: Vec<SignatureExposure>,
    attr: &syn::Attribute,
    cx: &LocalFactCtx,
    plugin: &'static str,
    rule: &'static str,
    out: &mut Vec<Fact>,
) {
    let trigger = cx.span(attr.path().span());
    out.extend(exps.into_iter().map(|exp| Fact::Exposure {
        exp,
        by: Provenance {
            plugin,
            rule,
            trigger: trigger.clone(),
        },
    }));
}

/// Consume an unrecognized nested-meta entry's payload (`= value` or `(group)`) so
/// [`syn::Attribute::parse_nested_meta`] can advance to the next sibling key. A
/// `= value` is read token-tree by token-tree up to the next top-level comma; safe
/// because the only value that can carry a top-level type comma (`into`/`error`) is
/// handled by its caller, never skipped here.
fn skip_meta_entry(meta: &syn::meta::ParseNestedMeta) -> syn::Result<()> {
    if meta.input.peek(syn::token::Paren) {
        return meta.parse_nested_meta(|_| Ok(()));
    }
    if meta.input.peek(syn::Token![=]) {
        let value = meta.value()?;
        while !value.is_empty() && !value.peek(syn::Token![,]) {
            value.parse::<proc_macro2::TokenTree>()?;
        }
    }
    Ok(())
}
