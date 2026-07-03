pub mod inner;

use inner::{kept};

/// Reached by the `app` bin (cross-crate) → stays alive and `pub`.
pub fn public_entry() -> u32 {
    kept()
}

