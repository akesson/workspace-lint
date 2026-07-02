//! No-op stand-in for derive_builder's derive: it only has to exist so the
//! fixture compiles — the lint under test reads the `#[builder(...)]` tokens
//! from source and never sees the (empty) expansion.
use proc_macro::TokenStream;

#[proc_macro_derive(Builder, attributes(builder))]
pub fn builder(_item: TokenStream) -> TokenStream {
    TokenStream::new()
}
