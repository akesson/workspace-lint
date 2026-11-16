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
