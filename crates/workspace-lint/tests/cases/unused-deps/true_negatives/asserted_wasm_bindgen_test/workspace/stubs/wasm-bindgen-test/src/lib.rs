//! Identity stand-in for `#[wasm_bindgen_test]`: keeps the annotated test fn
//! compiling; the real expansion's reference to `wasm-bindgen` is what the
//! built-in assertion models.
use proc_macro::TokenStream;

#[proc_macro_attribute]
pub fn wasm_bindgen_test(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}
