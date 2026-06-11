// The attribute arrives through an external-crate glob the resolver cannot see
// into; `wasm-bindgen` is required at expansion time but never named in source.
// The wasm-bindgen-test assertion credits it.
#[cfg(test)]
mod tests {
    use wasm_bindgen_test::*;

    #[wasm_bindgen_test]
    fn smoke() {
        assert_eq!(2 + 2, 4);
    }
}
