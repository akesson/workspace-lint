//! P5: a nested brace-list glob (`use a::{b, c::*}`) — pins the lowered
//! leaf's `decl_span` shape so the surgery guard's "whole statement or bail"
//! assumption stays verified.
pub mod sub {
    pub struct Gadget;

    pub mod inner {
        pub struct Thing;
    }
}

mod consumer {
    use crate::nested::sub::{Gadget, inner::*};

    pub fn touch() -> (Gadget, Thing) {
        (Gadget, Thing)
    }
}
