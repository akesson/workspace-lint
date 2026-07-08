//! P2 (the design gate): names resolved through the glob — a type by name, a
//! `macro_rules!` by invocation, a derive by attribute — must all appear in
//! the glob edge's `glob_used_names`.
use crate::prelude::*;

#[derive(ProbeDerive)]
pub struct Marked;

pub fn build() -> Widget {
    widget!()
}
