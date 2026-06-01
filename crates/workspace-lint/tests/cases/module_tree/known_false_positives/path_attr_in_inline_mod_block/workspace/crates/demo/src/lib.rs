// KNOWN FALSE POSITIVE (module_tree).
//
// `inner` carries a `#[path]` attribute while nested inside the inline block
// `mod outer`. Per Rust's module-path rules, a `#[path]` inside an inline block
// in a mod-rs file (lib.rs) is relative to the inline-module components as
// directories — so the backing file lives at `src/outer/custom_inner.rs`, and
// this crate is valid Rust.
//
// The resolver instead resolves `#[path]` relative to the *declaring file's*
// directory (`src/`), so it looks for `src/custom_inner.rs`, misses, and emits
// two spurious diagnostics: an "unresolved `mod inner`" and an "orphan file"
// for `custom_inner.rs`. When the resolver learns to anchor a nested-inline
// `#[path]` at the nested-module dir, both stop firing and this promotes to a
// true_negative.
pub mod outer {
    #[path = "custom_inner.rs"]
    pub mod inner;
}
