pub mod inner;

use inner::{helper, kept};

/// Reached by the `app` bin (cross-crate) → stays alive and `pub`.
pub fn public_entry() -> u32 {
    kept()
}

/// Nothing calls this → `Unused` → deleted whole-item (doc comment through
/// body). It is the only caller of `helper`, so `helper` cascades dead too.
pub fn dead_a() -> u32 {
    helper()
}
