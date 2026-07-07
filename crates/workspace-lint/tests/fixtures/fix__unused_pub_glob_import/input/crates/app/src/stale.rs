//! A pre-existing unused glob: no deletion in this module caused it, so it
//! stays the author's business (causality) — rustc's own `unused_imports`
//! already points at it.
#![allow(unused_imports)]
use demo::prelude::*;

pub fn keep() -> i32 {
    3
}
