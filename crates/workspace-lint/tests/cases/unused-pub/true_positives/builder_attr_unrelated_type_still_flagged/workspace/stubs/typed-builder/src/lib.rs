//! No-op stand-in for typed_builder's derive: it only has to exist so the
//! fixture compiles — the lint under test reads the `#[builder(...)]` tokens
//! from source and never sees the (empty) expansion.
use proc_macro::TokenStream;

#[proc_macro_derive(TypedBuilder, attributes(builder))]
pub fn typed_builder(_item: TokenStream) -> TokenStream {
    TokenStream::new()
}
