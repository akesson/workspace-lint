use demo::prelude::*;

/// Deleted: unused.
pub fn dead() -> Widget {
    Widget
}

/// Survives — and its `widget!` invocation is glob-supplied, so the glob
/// must stay.
pub fn build() -> Widget {
    widget!()
}
