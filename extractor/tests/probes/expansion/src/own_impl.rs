//! `RefEdge::own_impl` probes (schema 14). Each `Own*` type is referenced
//! ONLY from inside its own impls; `Host`'s impl names `OwnTarget` and must
//! not be flagged.

/// Inherent impl (receiver signature edge + header) and a hand-written trait
/// impl — both `Written`.
pub struct OwnTarget;

impl OwnTarget {
    pub fn by_ref(&self) -> u32 {
        1
    }
}

// Not `Display::fmt`: probe check 1 treats every trait-impl `clone`/`fmt`
// in this crate as derive output.
impl core::ops::Not for OwnTarget {
    type Output = u32;
    fn not(self) -> u32 {
        0
    }
}

/// Derive-only — `Derived`.
#[derive(Clone, PartialEq)]
pub struct OwnDerived;

/// Custom-derive-only (the `EnumStringify` shape) — `Derived`.
#[derive(procmacro::ProbeInherent)]
pub struct OwnCustomDerived;

/// Carries a derive whose impl is for `OwnVictim`, not for itself.
#[derive(procmacro::ProbeElsewhere)]
pub struct OwnDeriveHost;

/// Gets its only impl from `OwnDeriveHost`'s derive — `Written`: deleting
/// this type leaves that derive emitting an impl of a missing type.
pub struct OwnVictim;

/// Custom derive spanning its output with the input's tokens — `Derived`.
#[derive(procmacro::ProbeSpanned)]
pub struct OwnSpannedDerived;

/// Custom derive emitting its impl through a helper macro — `Derived`.
#[derive(procmacro::ProbeViaMacro)]
pub struct OwnViaMacroDerived;

/// Re-emits its attributes onto a macro-made struct: the derive path is
/// source text, but the item it lands on is not.
macro_rules! wrap {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])* pub struct $name;
    };
}

wrap!(#[derive(procmacro::ProbeNext)] OwnWrapped);

/// The first SOURCE ADT after the wrapped derive's path, and the self type
/// of the impl that derive emits — `Written`: the derive is not its own.
pub struct OwnWrapVictim;

/// Hand-written, merely carrying the marker — `Written`.
pub struct OwnAutoMarked;

#[automatically_derived]
impl core::ops::Neg for OwnAutoMarked {
    type Output = u32;
    fn neg(self) -> u32 {
        0
    }
}

/// A built-in derive on a macro-made type: no source item to delete with
/// it — `Written`.
macro_rules! make_derived {
    ($name:ident) => {
        #[derive(Clone)]
        pub struct $name;
    };
}

make_derived!(OwnMacroMade);

/// An EMPTY inherent impl: its header is the only evidence the impl exists.
/// The pre-14 display-path dedup dropped exactly this edge.
pub struct OwnEmpty;

impl OwnEmpty {}

/// Referenced only from an impl of a NESTED helper type inside its own impl
/// — the serde `Deserialize` shape (`__Visitor`'s `impl Visitor` names the
/// target). Still own-impl: the climb passes through `Helper`'s impl.
pub struct OwnNested;

impl OwnNested {
    pub fn count() -> u32 {
        struct Helper;
        impl Helper {
            fn build(&self) -> OwnNested {
                OwnNested
            }
        }
        let _ = Helper.build();
        1
    }
}

/// Another type's impl naming `OwnTarget` — a real reference, not own-impl.
pub struct Host;

impl From<OwnTarget> for Host {
    fn from(_: OwnTarget) -> Self {
        Host
    }
}
