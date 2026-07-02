//! No-op stand-in for strum_macros' `EnumString` derive: it only has to exist
//! so the fixture compiles; the real expansion's reference to `strum` is what
//! the built-in assertion models.
use proc_macro::TokenStream;

#[proc_macro_derive(EnumString)]
pub fn enum_string(_item: TokenStream) -> TokenStream {
    TokenStream::new()
}
