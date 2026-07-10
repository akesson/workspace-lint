//! Signature-exposure probes for the predicate/bounds family (probe.rs
//! check 23). Every `Bound*` trait and `Only*` type here is named in a
//! signature position that `fn_sig`/`type_of` alone cannot see — the def
//! lives in a predicate (bounds, where-clauses, supertraits, item bounds)
//! or in a field type — so each edge below exists only if the predicate
//! sweep emits it. Tightening any of these to `pub(crate)` while its
//! carrier stays `pub` is E0445/E0446: the `in_signature` edge is what
//! stops unused-pub from proposing exactly that.

// Class 1: inline generic bound — the reported blind spot (the `coalesce<R:
// ByteRange + Copy>` shape that shipped behind an expect! directive).
pub trait BoundInline {}
pub fn takes_inline_bound<R: BoundInline + Copy>(_r: R) -> u32 {
    1
}

// Class 2: `where` clause (same predicate store, distinct written surface).
pub trait BoundWhere {}
pub fn takes_where_bound<R>(_r: R) -> u32
where
    R: BoundWhere,
{
    2
}

// Class 3: argument-position `impl Trait` — desugars to a synthetic generic
// param whose bound lands in the fn's own predicates.
pub trait BoundApit {}
pub fn takes_apit(_r: impl BoundApit) -> u32 {
    3
}

// Class 4: return-position `impl Trait` — the opaque's bounds live in its
// `explicit_item_bounds`, not in `fn_sig`'s output type tree.
pub trait BoundRpit {}
impl BoundRpit for u32 {}
pub fn returns_rpit() -> impl BoundRpit {
    4u32
}

// Class 4b: NESTED opaque (`impl Iterator<Item = impl …>`) — the inner
// opaque is reachable only through the outer one's item bounds, so the
// drain must recurse.
pub trait BoundRpitNested {}
impl BoundRpitNested for u32 {}
pub fn returns_nested_rpit() -> impl Iterator<Item = impl BoundRpitNested> {
    std::iter::once(5u32)
}

// Class 5: supertrait — the `Trait` arm of the signature pass was empty.
pub trait BoundSuper {}
pub trait HasSuper: BoundSuper {}

// Class 6: trait-decl associated-type bound — the assoc type has no
// `type_of` (that query is impl-side only); the bound is its item bounds.
pub trait BoundAssoc {}
pub trait HasAssoc {
    type Item: BoundAssoc;
}

// Class 7: `dyn Trait` in a signature — a trait def inside a `ty::Dynamic`
// is not an `Adt` and not a projection, so the old visitor never saw it.
pub trait BoundDyn {}
pub fn takes_dyn(_d: &dyn BoundDyn) -> u32 {
    6
}

// Class 9: field types. Fields are not in `definitions()`, so the old pass
// never walked their `type_of` at all. The edge's `from` must be the FIELD
// def (not the ADT): a private field's type is legitimately tightenable,
// and only field-level `from` lets the assembler gate on field visibility.
pub struct OnlyFieldType(pub u32);
pub struct HasField {
    pub named: OnlyFieldType,
}
pub struct PrivInner(pub u32);
pub struct HasPrivField {
    hidden: PrivInner,
}
impl HasPrivField {
    pub fn get(&self) -> u32 {
        self.hidden.0
    }
}

// Class 9b: enum variant fields reach the same sweep via `all_fields()`.
pub struct OnlyEnumFieldType(pub u32);
pub enum HasVariants {
    Carry(OnlyEnumFieldType),
}
pub fn touch(v: HasVariants) -> u32 {
    match v {
        HasVariants::Carry(x) => x.0 + takes_inline_bound(0u32) + takes_where_bound(0u32),
    }
}
pub fn touch_more() -> u32 {
    let _opaque = returns_rpit();
    takes_apit(Apit)
        + takes_dyn(&Dyn)
        + returns_nested_rpit().next().map_or(0, |_| 1)
        + HasField { named: OnlyFieldType(7) }.named.0
        + HasPrivField { hidden: PrivInner(8) }.get()
}

// Minimal impls so the probes above are call-able (keeps the crate honest
// under `-D warnings`/dead-code and gives the bounds concrete witnesses).
pub struct Apit;
impl BoundApit for Apit {}
pub struct Dyn;
impl BoundDyn for Dyn {}
impl BoundInline for u32 {}
impl BoundWhere for u32 {}
