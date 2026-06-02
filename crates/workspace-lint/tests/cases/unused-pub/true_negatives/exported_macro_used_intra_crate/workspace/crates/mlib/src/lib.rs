// TRUE NEGATIVE (unused-pub) — exported `macro_rules!` used only intra-crate.
//
// `helper` is a `#[macro_export] macro_rules!` (modeled as a *public* item)
// defined here and invoked only as a bare `helper!(...)` within this same crate.
// A bare single-ident macro invocation isn't a `use`-binding or a sibling —
// macros are excluded from siblings so a local `macro_rules! log` can't shadow
// the external `log` crate in a `log::debug!` path — so without the core Phase B
// `MacroCallPass` the macro has zero referrers and reads "appears unused", the
// false positive this fixture guards against. The pass binds the bare invocation
// to the same-crate definition, making it IntraCrate (suppressed below).

#[macro_export]
macro_rules! helper {
    ($x:expr) => {
        $x + 1
    };
}

// Private (so it isn't itself a finding); its body is the only referrer of
// `helper`, via a bare single-ident invocation.
fn _anchor() -> i32 {
    helper!(41)
}
