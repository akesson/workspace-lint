use demo::prelude::*;

/// Survives — `.shout()` resolves only because the glob puts `Shout` in
/// scope (the type is named by full path, not through the glob).
pub(crate) fn speak() -> &'static str {
    demo::prelude::Widget.shout()
}
