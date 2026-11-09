//! Minimal stand-in for derive_builder's derive. It models the one property
//! the fixture is about: the expansion contains a generated PUBLIC fn whose
//! signature names the `#[builder(build_fn(error = "…"))]` type, exactly as
//! the real derive's `pub fn build(&self) -> Result<_, Error>` does. The
//! engine sees the expansion (it extracts inside rustc), so that signature
//! is what exposes the error type — no attribute parsing in the lint.
use proc_macro::TokenStream;

#[proc_macro_derive(Builder, attributes(builder))]
pub fn builder(item: TokenStream) -> TokenStream {
    // The fixture's only string literal is the `error = "…"` path.
    let src = item.to_string();
    let error = src.split('"').nth(1).expect("build_fn(error = \"…\")");
    format!(
        "pub fn __derive_builder_build() -> ::core::result::Result<(), {error}> {{ \
         ::core::unimplemented!() }}"
    )
    .parse()
    .expect("valid tokens")
}
