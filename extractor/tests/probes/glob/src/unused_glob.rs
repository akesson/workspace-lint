//! P1 (empty side): a glob the resolver never used — `glob_used_names` must
//! be empty (and its `decl_span` still present: the delete surface exists,
//! the accounting's causality rule R1 is what protects it).
#![allow(unused_imports)]
use crate::prelude::*;

pub fn nothing() -> i32 {
    0
}
