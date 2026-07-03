//! Inert stand-ins for the Dioxus macros the fixtures name. The lint under
//! test recognizes `rsx!` / `#[component]` / `#[derive(Routable)]` by name in
//! the source text; these exist only so the workspace compiles.
use proc_macro::TokenStream;

/// Identity attribute: keeps the annotated `pub fn` compiling unchanged.
#[proc_macro_attribute]
pub fn component(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// Swallows the rsx body and expands to `()` so it works in expression position.
#[proc_macro]
pub fn rsx(_input: TokenStream) -> TokenStream {
    "()".parse().expect("unit expr")
}

/// No-op derive accepting the router's `#[route(...)]` / `#[layout(...)]`
/// helper attributes.
#[proc_macro_derive(Routable, attributes(route, layout))]
pub fn routable(_item: TokenStream) -> TokenStream {
    TokenStream::new()
}
