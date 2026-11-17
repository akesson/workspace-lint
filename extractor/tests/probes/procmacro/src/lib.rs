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

/// `ProbeInherent`'s impl with every token spanned at the input's type name
/// (the `quote_spanned!(input.ident.span()=> …)` shape): the impl's own
/// span has no expansion context, so only the defining expansion says it
/// came from a derive.
#[proc_macro_derive(ProbeSpanned)]
pub fn probe_spanned(item: TokenStream) -> TokenStream {
    let (name, span) = adt_ident(item);
    let out: TokenStream = format!("impl {name} {{}}").parse().unwrap();
    respan(out, span)
}

/// Emits its impl through a helper `macro_rules!` (the `impl_x!(T)`-inside-
/// a-derive shape): the impl's defining expansion is the helper's, a child
/// of the derive's.
#[proc_macro_derive(ProbeViaMacro)]
pub fn probe_via_macro(item: TokenStream) -> TokenStream {
    let name = adt_name(item);
    format!(
        "macro_rules! __probe_impl {{ ($t:ident) => {{ impl $t {{}} }} }} __probe_impl!({name});"
    )
    .parse()
    .unwrap()
}

/// Like `ProbeElsewhere`, for the macro-wrapped host (`wrap!` in
/// own_impl.rs): the impl is for `OwnWrapVictim`, written right after it.
#[proc_macro_derive(ProbeNext)]
pub fn probe_next(_item: TokenStream) -> TokenStream {
    "impl crate::own_impl::OwnWrapVictim {}".parse().unwrap()
}

fn respan(ts: TokenStream, span: proc_macro::Span) -> TokenStream {
    ts.into_iter()
        .map(|mut tt| {
            if let proc_macro::TokenTree::Group(g) = &tt {
                let mut inner = proc_macro::Group::new(g.delimiter(), respan(g.stream(), span));
                inner.set_span(span);
                tt = proc_macro::TokenTree::Group(inner);
            }
            tt.set_span(span);
            tt
        })
        .collect()
}

/// The identifier after `struct` / `enum` / `union` (no `syn` dep).
fn adt_name(item: TokenStream) -> String {
    adt_ident(item).0
}

fn adt_ident(item: TokenStream) -> (String, proc_macro::Span) {
    let mut after_kw = false;
    for tt in item {
        if let proc_macro::TokenTree::Ident(id) = tt {
            let s = id.to_string();
            if after_kw {
                return (s, id.span());
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
