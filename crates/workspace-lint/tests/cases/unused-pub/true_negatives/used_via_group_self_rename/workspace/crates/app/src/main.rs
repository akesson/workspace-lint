// `{self as st}` — a group-self import WITH a rename. The binding must
// resolve `st` to the module `corelib::styles` (not a bogus `styles::self`),
// so `st::filter()` counts as a cross-crate use of `filter`.
use corelib::styles::{self as st};

fn main() {
    let _ = st::filter();
}
