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

// The macro must be INVOKED for the reference to exist under compiler
// semantics: an uninvoked `macro_rules!` body is never expanded, so its paths
// resolve nowhere (the real memchr this mirrors invokes `debug!` throughout).
// The invocation is what puts the `log::debug!` edge in post-expansion HIR;
// the namespace question — the local `macro_rules! log` must not shadow the
// module-namespace `log` crate — is exercised identically on both backends.
pub fn touch() {
    debug!("exercised");
}
