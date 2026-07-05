use proc_macro::TokenStream;

#[proc_macro_derive(Unused)]
pub fn derive_unused(_item: TokenStream) -> TokenStream {
    TokenStream::new()
}
