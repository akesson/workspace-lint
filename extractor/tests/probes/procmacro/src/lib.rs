//! One entry point per proc-macro flavor; each must carry the `proc_macro`
//! export attr in the emitted fragment. Bodies are the identity transform —
//! the probe asserts attrs, not expansion behavior.

use proc_macro::TokenStream;

#[proc_macro_derive(ProbeDerive)]
pub fn probe_derive(_item: TokenStream) -> TokenStream {
    TokenStream::new()
}

/// The custom-derive shape (`EnumStringify`): an inherent impl of the
/// annotated type — `ImplOrigin::Derived`, deleted with the type (probe.rs
/// check 26).
#[proc_macro_derive(ProbeInherent)]
pub fn probe_inherent(item: TokenStream) -> TokenStream {
    format!("impl {} {{ pub fn derived_probe() -> u32 {{ 0 }} }}", adt_name(item))
        .parse()
        .unwrap()
}

/// A derive emitting an impl of ANOTHER type: deleting that type leaves the
/// derive regenerating an impl of a missing type, so it stays `Written`.
#[proc_macro_derive(ProbeElsewhere)]
pub fn probe_elsewhere(_item: TokenStream) -> TokenStream {
    "impl crate::own_impl::OwnVictim {}".parse().unwrap()
}

/// The identifier after `struct` / `enum` / `union` (no `syn` dep).
fn adt_name(item: TokenStream) -> String {
    let mut after_kw = false;
    for tt in item {
        if let proc_macro::TokenTree::Ident(id) = tt {
            let s = id.to_string();
            if after_kw {
                return s;
            }
            after_kw = matches!(s.as_str(), "struct" | "enum" | "union");
        }
    }
    panic!("derive input has no ADT name")
}

#[proc_macro]
pub fn probe_bang(item: TokenStream) -> TokenStream {
    item
}

#[proc_macro_attribute]
pub fn probe_attr(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// A plain helper next to the entries — must NOT get the `proc_macro` attr
/// (the root is per-item, not per-crate). Private: a proc-macro crate cannot
/// export anything but its entry points.
#[allow(dead_code)]
fn plain_helper() {}
