//! Plugin: `#[wasm_bindgen_test]` implies a reference to the `wasm-bindgen` runtime.
//!
//! The attribute expands to code requiring the `wasm_bindgen` runtime crate, and
//! usually arrives via `use wasm_bindgen_test::*;` — so it's matched by the attribute
//! path's last segment, covering both the bare and fully-qualified forms.

use super::{Trigger, UsageAssertion, scan};
use crate::plugins::{Fact, LocalFactCtx, ResolverPlugin};

pub(crate) const WASM_BINDGEN_TEST: UsageAssertion = UsageAssertion {
    id: "wasm-bindgen-test",
    trigger: Trigger::AttrPath {
        idents: &["wasm_bindgen_test"],
    },
    implies: &["wasm_bindgen"],
    citation: "https://rustwasm.github.io/wasm-bindgen/wasm-bindgen-test/usage.html",
};

pub(crate) struct WasmBindgenTestPlugin;

impl ResolverPlugin for WasmBindgenTestPlugin {
    fn local_facts(&self, item: &syn::Item, cx: &LocalFactCtx) -> Vec<Fact> {
        scan(&WASM_BINDGEN_TEST, "wasm_bindgen", item, cx)
    }
}
