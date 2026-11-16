use std::fmt;

/// Unused, with a hand-written impl: deleting it alone would orphan the
/// `impl fmt::Display` — vetoed.
pub struct Orphanable;

impl fmt::Display for Orphanable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("orphanable")
    }
}

/// Unused, derives only — deleted together with its derives.
#[derive(Debug, Clone, PartialEq)]
pub struct DerivedOnly {
    pub x: u32,
}

/// Mentioned nowhere — deleted.
pub fn dead_helper() -> i32 {
    0
}

/// Used by `app`.
pub fn live() -> i32 {
    1
}
