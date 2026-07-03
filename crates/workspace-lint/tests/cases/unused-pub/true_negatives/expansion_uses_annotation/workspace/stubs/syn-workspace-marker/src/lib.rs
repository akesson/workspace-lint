//! Mirror of the real marker macro: expands to nothing; syn-workspace reads
//! the invocation out of the source tree (Layer 2 annotation parsing).
#[macro_export]
macro_rules! expansion_uses {
    ($($tt:tt)*) => {};
}
