use demo::prelude::*;

/// Deleted: unused (and touching nothing glob-supplied).
pub fn dead() -> i32 {
    2
}

/// Survives — `.shout()` resolves only because the glob puts `Shout` in
/// scope (the type is named by full path, not through the glob).
pub fn speak() -> &'static str {
    demo::prelude::Widget.shout()
}
