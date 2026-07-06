use std::fmt::Write;
use util::{gadget};

/// Reached by the `app` bin (cross-crate) → stays alive and `pub`.
pub fn entry() -> u32 {
    1
}

/// Unused → deleted; the last user of both imports above and of `helper`.
pub fn dead() -> String {
    let mut s = String::new();
    let _ = write!(s, "{}", helper() + gadget());
    s
}

fn helper() -> u32 {
    inner() + 1
}

fn inner() -> u32 {
    2
}
