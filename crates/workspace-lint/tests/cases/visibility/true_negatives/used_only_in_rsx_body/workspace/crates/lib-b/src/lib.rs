// No `use lib_a::Button;` — the only reference to lib_a::Button lives
// inside the rsx! { ... } body below. The token-scanner inside
// extract_code_paths handles function-body macro invocations by recursing
// into the group, where `lib_a :: Button` matches the multi-segment
// pattern and gets recorded as a reference.

pub(crate) fn render() {
    rsx! {
        lib_a::Button { }
    }
}

// Local stub of `rsx!` so the file parses with bare `syn` (the resolver
// uses cargo metadata, which doesn't compile the file — but a future
// stricter checker might). Keep this trivial; we don't ship dioxus here.
macro_rules! rsx {
    ($($body:tt)*) => {};
}
pub(crate) use rsx;
