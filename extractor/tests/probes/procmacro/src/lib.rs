//! One entry point per proc-macro flavor; each must carry the `proc_macro`
//! export attr in the emitted fragment. Bodies are the identity transform —
//! the probe asserts attrs, not expansion behavior.

use proc_macro::TokenStream;

#[proc_macro_derive(ProbeDerive)]
pub fn probe_derive(_item: TokenStream) -> TokenStream {
    TokenStream::new()
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
