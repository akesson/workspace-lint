// TRUE NEGATIVE (module_tree) — `#[path]` inside an inline `mod` block.
//
// `inner` carries a `#[path]` attribute while nested inside the inline block
// `mod outer`. Per Rust's module-path rules, a `#[path]` inside an inline block
// in a mod-rs file (lib.rs) is relative to the inline-module components as
// directories — so the backing file lives at `src/outer/custom_inner.rs`, and
// this crate is valid Rust.
//
// The resolver anchors a nested-inline `#[path]` at that nested-module dir
// (`resolve_mod_file`'s `in_inline` case), finds `src/outer/custom_inner.rs`,
// resolves `mod inner`, and sees the file as reachable — no diagnostics. (Was a
// tracked false positive that emitted a spurious "unresolved `mod inner`" +
// "orphan file" pair back when `#[path]` was anchored at the declaring file's
// dir; fixed and promoted here.)
pub mod outer {
    #[path = "custom_inner.rs"]
    pub mod inner;
}
