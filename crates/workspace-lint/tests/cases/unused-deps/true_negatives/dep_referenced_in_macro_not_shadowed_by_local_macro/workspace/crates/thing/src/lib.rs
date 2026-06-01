// Regression guard (mirrors memchr 2.8.1 `src/macros.rs`): a local
// `macro_rules! log` introduces `log` in the *macro* namespace only. The path
// `log::debug` inside the `debug!` macro body resolves `log` in the
// *type/module* namespace, where the local macro does not participate — so it
// is a genuine reference to the external `log` crate. The resolver must not let
// the macro namesake shadow it, or `unused-deps` falsely flags `log`.
#![allow(unused_macros)]

macro_rules! log {
    ($($tt:tt)*) => {
        $($tt)*
    };
}

macro_rules! debug {
    ($($tt:tt)*) => {
        log::debug!($($tt)*)
    };
}
