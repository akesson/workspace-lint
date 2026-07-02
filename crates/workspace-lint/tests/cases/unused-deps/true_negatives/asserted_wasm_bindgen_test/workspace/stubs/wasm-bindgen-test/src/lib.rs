//! Stand-in for `#[wasm_bindgen_test]` with the real expansion's load-bearing
//! shape: the emitted scaffolding references `wasm_bindgen`, so that dep is
//! exercised only through generated code — the premise of the fixture. (The
//! syn backend can't expand and needs its name-keyed assertion; the rustc
//! backend sees the edge natively in post-expansion HIR.)
use proc_macro::TokenStream;

#[proc_macro_attribute]
pub fn wasm_bindgen_test(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut out: TokenStream = "const _: () = wasm_bindgen::EXPANSION_MARKER;"
        .parse()
        .unwrap();
    out.extend(item);
    out
}
