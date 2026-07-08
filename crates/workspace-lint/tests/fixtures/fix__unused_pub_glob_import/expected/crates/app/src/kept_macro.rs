use demo::prelude::*;

/// Survives — and its `widget!` invocation is glob-supplied, so the glob
/// must stay.
pub(crate) fn build() -> Widget {
    widget!()
}
