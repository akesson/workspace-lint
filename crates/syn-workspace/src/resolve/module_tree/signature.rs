//! Signature-exposure walk: an AST-aware pass that collects the type paths
//! appearing in the *public signature surface* of each item.
//!
//! Separate from the token-based occurrence scan in [`super::occurrences`],
//! which flattens an item to tokens and so cannot tell a signature position
//! from a body. Here we keep the `syn` AST and walk only the signature surface —
//! fn parameter/return types, `pub` field types, trait-impl associated-type
//! values, type-alias RHSs, const/static types, trait-item signatures, and the
//! generic bounds / `where`-clauses of all of those — recursing into nested
//! generic arguments (`Vec<Foo>`, `Result<Foo>`). Each type path is resolved
//! through the *same* [`resolve_code_path`] the occurrence scan uses, so the
//! canonicals it produces line up with the reference graph.
//!
//! The result feeds [`Workspace::exposed_in_public_signature`](crate::Workspace::exposed_in_public_signature),
//! which `unused-pub` consults to avoid suggesting a `pub(crate)` tighten on a
//! type that a more-visible item exposes (E0446 / `private_interfaces`).
//!
//! ## Visibility policy
//!
//! Only `Public`-exposed types matter to the binary query, so we record an
//! exposure only when the enclosing item is reachable at `Public`. Two
//! deliberate approximations, both FP-safe (they can only ever *suppress* an
//! unsafe tighten, never invent a finding):
//!
//! - **Trait-impl members** carry no `vis` of their own; their signature types
//!   are conservatively treated as `Public`. (The precise bound is the impl's
//!   reachability — the self type's visibility — but resolving that is a later
//!   refinement; over-recording here is safe.)
//! - **Inherent-impl members** use their own `vis`. (Slightly over-records a
//!   `pub fn` inside an `impl` of a `pub(crate)` type, which is FP-safe.)
//!
//! The impl's self type and the implemented trait path are deliberately *not*
//! recorded: an `impl` block doesn't force its self type (or a sealed trait) to
//! stay public, so recording them would over-suppress nearly every impl'd type.

use std::collections::HashSet;

use super::occurrences::resolve_code_path;
use super::use_tree::{self, UseBinding};
use super::{ResolvedPath, SignatureExposure, Visibility};

/// Shared, read-only resolution context threaded through the walk.
struct Ctx<'a> {
    scope: &'a use_tree::Scope,
    siblings: &'a HashSet<String>,
    use_bindings: &'a [UseBinding],
    parent_canonical: &'a ResolvedPath,
}

/// Walk one top-level item's public signature surface, pushing a
/// [`SignatureExposure`] for every type path reachable through a `Public`
/// signature position.
pub(super) fn collect_signature_exposures(
    syn_item: &syn::Item,
    scope: &use_tree::Scope,
    siblings: &HashSet<String>,
    use_bindings: &[UseBinding],
    parent_canonical: &ResolvedPath,
    out: &mut Vec<SignatureExposure>,
) {
    let ctx = Ctx {
        scope,
        siblings,
        use_bindings,
        parent_canonical,
    };
    // Thin dispatcher: each item kind's signature surface lives in its own
    // walker below, so this match stays a flat fan-out (one arm = one kind).
    match syn_item {
        syn::Item::Fn(f) => walk_item_fn(f, &ctx, out),
        syn::Item::Struct(s) => walk_item_struct(s, &ctx, out),
        syn::Item::Enum(e) => walk_item_enum(e, &ctx, out),
        syn::Item::Union(u) => walk_item_union(u, &ctx, out),
        syn::Item::Type(t) => walk_item_type_alias(t, &ctx, out),
        syn::Item::Const(c) => walk_public_typed(&c.vis, &c.ty, &ctx, out),
        syn::Item::Static(s) => walk_public_typed(&s.vis, &s.ty, &ctx, out),
        syn::Item::Trait(tr) => walk_item_trait(tr, &ctx, out),
        syn::Item::Impl(imp) => walk_item_impl(imp, &ctx, out),
        // Other item kinds (Mod, Use, ExternCrate, Macro, …) expose no signature.
        _ => {}
    }
}

fn walk_item_fn(f: &syn::ItemFn, ctx: &Ctx, out: &mut Vec<SignatureExposure>) {
    let vis = Visibility::from_syn(&f.vis);
    if is_public(vis) {
        walk_signature(&f.sig, vis, ctx, out);
    }
}

fn walk_item_struct(s: &syn::ItemStruct, ctx: &Ctx, out: &mut Vec<SignatureExposure>) {
    let vis = Visibility::from_syn(&s.vis);
    if !is_public(vis) {
        return;
    }
    walk_generics(&s.generics, vis, ctx, out);
    // A field leaks only when the field itself is `pub` (its effective
    // visibility is the more restrictive of struct and field — both must be
    // public to expose at `Public`).
    for field in &s.fields {
        if is_public(Visibility::from_syn(&field.vis)) {
            walk_type(&field.ty, vis, ctx, out);
        }
    }
}

fn walk_item_enum(e: &syn::ItemEnum, ctx: &Ctx, out: &mut Vec<SignatureExposure>) {
    let vis = Visibility::from_syn(&e.vis);
    if !is_public(vis) {
        return;
    }
    walk_generics(&e.generics, vis, ctx, out);
    // Enum variant fields have no individual visibility — they are as visible
    // as the enum itself.
    for variant in &e.variants {
        for field in &variant.fields {
            walk_type(&field.ty, vis, ctx, out);
        }
    }
}

fn walk_item_union(u: &syn::ItemUnion, ctx: &Ctx, out: &mut Vec<SignatureExposure>) {
    let vis = Visibility::from_syn(&u.vis);
    if !is_public(vis) {
        return;
    }
    walk_generics(&u.generics, vis, ctx, out);
    for field in &u.fields.named {
        if is_public(Visibility::from_syn(&field.vis)) {
            walk_type(&field.ty, vis, ctx, out);
        }
    }
}

fn walk_item_type_alias(t: &syn::ItemType, ctx: &Ctx, out: &mut Vec<SignatureExposure>) {
    let vis = Visibility::from_syn(&t.vis);
    if !is_public(vis) {
        return;
    }
    walk_type(&t.ty, vis, ctx, out);
    walk_generics(&t.generics, vis, ctx, out);
}

/// Shared walker for the `const`/`static` shape (a single typed item whose own
/// `vis` gates exposure).
fn walk_public_typed(
    vis: &syn::Visibility,
    ty: &syn::Type,
    ctx: &Ctx,
    out: &mut Vec<SignatureExposure>,
) {
    let vis = Visibility::from_syn(vis);
    if is_public(vis) {
        walk_type(ty, vis, ctx, out);
    }
}

fn walk_item_trait(tr: &syn::ItemTrait, ctx: &Ctx, out: &mut Vec<SignatureExposure>) {
    let vis = Visibility::from_syn(&tr.vis);
    if !is_public(vis) {
        return;
    }
    walk_generics(&tr.generics, vis, ctx, out);
    walk_bounds(&tr.supertraits, vis, ctx, out);
    for item in &tr.items {
        walk_trait_item(item, vis, ctx, out);
    }
}

fn walk_trait_item(
    item: &syn::TraitItem,
    vis: Visibility,
    ctx: &Ctx,
    out: &mut Vec<SignatureExposure>,
) {
    match item {
        syn::TraitItem::Fn(f) => walk_signature(&f.sig, vis, ctx, out),
        syn::TraitItem::Type(t) => {
            walk_bounds(&t.bounds, vis, ctx, out);
            if let Some((_, default)) = &t.default {
                walk_type(default, vis, ctx, out);
            }
            walk_generics(&t.generics, vis, ctx, out);
        }
        syn::TraitItem::Const(c) => walk_type(&c.ty, vis, ctx, out),
        _ => {}
    }
}

fn walk_item_impl(imp: &syn::ItemImpl, ctx: &Ctx, out: &mut Vec<SignatureExposure>) {
    // Trait impls force their members to match the impl's reachability (E0446);
    // treat them conservatively as `Public`. Inherent-impl members carry their
    // own visibility. The self type and trait path are intentionally not
    // recorded (see module docs).
    let is_trait_impl = imp.trait_.is_some();
    for item in &imp.items {
        walk_impl_item(item, is_trait_impl, ctx, out);
    }
}

fn walk_impl_item(
    item: &syn::ImplItem,
    is_trait_impl: bool,
    ctx: &Ctx,
    out: &mut Vec<SignatureExposure>,
) {
    match item {
        syn::ImplItem::Fn(f) => {
            let vis = member_vis(is_trait_impl, &f.vis);
            if is_public(vis) {
                walk_signature(&f.sig, vis, ctx, out);
            }
        }
        // The associated-type RHS — the E0446 trigger
        // (`type Response = JsonResponse<MockJoke>`).
        syn::ImplItem::Type(t) => {
            let vis = member_vis(is_trait_impl, &t.vis);
            if is_public(vis) {
                walk_type(&t.ty, vis, ctx, out);
                walk_generics(&t.generics, vis, ctx, out);
            }
        }
        syn::ImplItem::Const(c) => {
            let vis = member_vis(is_trait_impl, &c.vis);
            if is_public(vis) {
                walk_type(&c.ty, vis, ctx, out);
            }
        }
        _ => {}
    }
}

/// A trait-impl member is conservatively `Public`; an inherent-impl member
/// uses its own declared visibility.
fn member_vis(is_trait_impl: bool, vis: &syn::Visibility) -> Visibility {
    if is_trait_impl {
        Visibility::Public
    } else {
        Visibility::from_syn(vis)
    }
}

fn is_public(vis: Visibility) -> bool {
    matches!(vis, Visibility::Public)
}

/// Walk a fn signature: parameter types (skipping `self`), the return type,
/// and the generic bounds / where-clause.
fn walk_signature(
    sig: &syn::Signature,
    vis: Visibility,
    ctx: &Ctx,
    out: &mut Vec<SignatureExposure>,
) {
    for input in &sig.inputs {
        if let syn::FnArg::Typed(pat_type) = input {
            walk_type(&pat_type.ty, vis, ctx, out);
        }
    }
    if let syn::ReturnType::Type(_, ty) = &sig.output {
        walk_type(ty, vis, ctx, out);
    }
    walk_generics(&sig.generics, vis, ctx, out);
}

/// Walk generic-parameter bounds and the `where`-clause of any item that
/// carries [`syn::Generics`].
fn walk_generics(
    generics: &syn::Generics,
    vis: Visibility,
    ctx: &Ctx,
    out: &mut Vec<SignatureExposure>,
) {
    for param in &generics.params {
        if let syn::GenericParam::Type(tp) = param {
            walk_bounds(&tp.bounds, vis, ctx, out);
            // A default type argument (`T = inner::Secret`) is a signature
            // position too. Const-generic defaults are *values* and a const
            // param's type must be integral/bool/char, so it can't name a
            // private type — only `Type` defaults matter here.
            if let Some(default) = &tp.default {
                walk_type(default, vis, ctx, out);
            }
        }
    }
    if let Some(where_clause) = &generics.where_clause {
        for pred in &where_clause.predicates {
            if let syn::WherePredicate::Type(pt) = pred {
                walk_type(&pt.bounded_ty, vis, ctx, out);
                walk_bounds(&pt.bounds, vis, ctx, out);
            }
        }
    }
}

/// Walk a set of type-param bounds (`T: Bound + Other`, supertraits,
/// `dyn`/`impl Trait` bounds), recording each named trait path.
fn walk_bounds<'a, I>(bounds: I, vis: Visibility, ctx: &Ctx, out: &mut Vec<SignatureExposure>)
where
    I: IntoIterator<Item = &'a syn::TypeParamBound>,
{
    for bound in bounds {
        if let syn::TypeParamBound::Trait(tb) = bound {
            record_path(&tb.path, vis, ctx, out);
            for seg in &tb.path.segments {
                walk_path_arguments(&seg.arguments, vis, ctx, out);
            }
        }
    }
}

/// Recursively walk a type, recording every named type path it reaches —
/// including types nested inside generic arguments, references, tuples, and
/// `dyn`/`impl Trait` bounds.
fn walk_type(ty: &syn::Type, vis: Visibility, ctx: &Ctx, out: &mut Vec<SignatureExposure>) {
    match ty {
        syn::Type::Path(tp) => {
            if let Some(qself) = &tp.qself {
                walk_type(&qself.ty, vis, ctx, out);
            }
            record_path(&tp.path, vis, ctx, out);
            for seg in &tp.path.segments {
                walk_path_arguments(&seg.arguments, vis, ctx, out);
            }
        }
        syn::Type::Reference(r) => walk_type(&r.elem, vis, ctx, out),
        syn::Type::Ptr(p) => walk_type(&p.elem, vis, ctx, out),
        syn::Type::Slice(s) => walk_type(&s.elem, vis, ctx, out),
        syn::Type::Array(a) => walk_type(&a.elem, vis, ctx, out),
        syn::Type::Group(g) => walk_type(&g.elem, vis, ctx, out),
        syn::Type::Paren(p) => walk_type(&p.elem, vis, ctx, out),
        syn::Type::Tuple(t) => {
            for elem in &t.elems {
                walk_type(elem, vis, ctx, out);
            }
        }
        syn::Type::TraitObject(to) => walk_bounds(&to.bounds, vis, ctx, out),
        syn::Type::ImplTrait(it) => walk_bounds(&it.bounds, vis, ctx, out),
        // A bare-fn pointer (`fn(A) -> B`) names `A`/`B` in signature position,
        // so walk its inputs and return type — the same surface the
        // `Parenthesized` path-argument arm walks for `Fn(A) -> B`.
        syn::Type::BareFn(bare) => {
            for input in &bare.inputs {
                walk_type(&input.ty, vis, ctx, out);
            }
            if let syn::ReturnType::Type(_, ty) = &bare.output {
                walk_type(ty, vis, ctx, out);
            }
        }
        // `Macro`, `Infer`, `Never`, `Verbatim`, … expose no named workspace
        // type (or one we can't resolve from the signature) — intentionally
        // skipped.
        _ => {}
    }
}

/// Walk the generic arguments attached to a path segment — angle-bracketed
/// (`<Foo, Bar>`, `<Item = Foo>`) or parenthesized (`Fn(Foo) -> Bar`).
fn walk_path_arguments(
    args: &syn::PathArguments,
    vis: Visibility,
    ctx: &Ctx,
    out: &mut Vec<SignatureExposure>,
) {
    match args {
        syn::PathArguments::AngleBracketed(ab) => {
            for arg in &ab.args {
                match arg {
                    syn::GenericArgument::Type(t) => walk_type(t, vis, ctx, out),
                    syn::GenericArgument::AssocType(at) => walk_type(&at.ty, vis, ctx, out),
                    _ => {}
                }
            }
        }
        syn::PathArguments::Parenthesized(pa) => {
            for input in &pa.inputs {
                walk_type(input, vis, ctx, out);
            }
            if let syn::ReturnType::Type(_, ty) = &pa.output {
                walk_type(ty, vis, ctx, out);
            }
        }
        syn::PathArguments::None => {}
    }
}

/// Resolve a single path through the shared [`resolve_code_path`] logic and, if
/// it resolves to a canonical, record it as a `vis`-level exposure. Paths that
/// resolve to nothing (bare prelude/local idents, primitives) are dropped — the
/// same behaviour the occurrence scan relies on.
fn record_path(path: &syn::Path, vis: Visibility, ctx: &Ctx, out: &mut Vec<SignatureExposure>) {
    let segments: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
    if let Some(canonical) = resolve_code_path(
        segments,
        ctx.scope,
        ctx.siblings,
        ctx.use_bindings,
        ctx.parent_canonical,
    ) {
        out.push(SignatureExposure {
            canonical,
            enclosing_vis: vis,
        });
    }
}
