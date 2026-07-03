//! Minimal stand-ins for the Dioxus macros the fixtures name. Under the rustc
//! engine what matters is the *expansion*: the real `rsx!` expands `Card {}`
//! to component-builder calls that reference `Card`, and the real `Routable`
//! derive expands `#[route("/", ..)]` / `#[layout(NavBar)]` variants to match
//! arms that reference each component fn. These stubs reproduce exactly that
//! load-bearing property — one `let _ = Ident;` per referenced component —
//! with hand-rolled token scans (no syn/quote: the fixture compiles offline).
use proc_macro::{Delimiter, TokenStream, TokenTree};

/// Identity attribute: keeps the annotated `pub fn` compiling unchanged.
#[proc_macro_attribute]
pub fn component(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// Expands to a block referencing every `Component { .. }` name in the body —
/// the same references the real macro's builder calls carry.
#[proc_macro]
pub fn rsx(input: TokenStream) -> TokenStream {
    let mut refs = String::new();
    collect_brace_targets(input, &mut refs);
    format!("{{ {refs} }}").parse().expect("rsx stub expansion parses")
}

/// `Ident` immediately followed by a `{ .. }` group — the bare-component shape
/// — anywhere in the (possibly nested) token stream.
fn collect_brace_targets(ts: TokenStream, refs: &mut String) {
    let mut prev_ident: Option<String> = None;
    for tt in ts {
        match tt {
            TokenTree::Ident(i) => prev_ident = Some(i.to_string()),
            TokenTree::Group(g) => {
                if g.delimiter() == Delimiter::Brace
                    && let Some(name) = prev_ident.take()
                {
                    refs.push_str(&format!("let _ = {name}; "));
                }
                collect_brace_targets(g.stream(), refs);
                prev_ident = None;
            }
            _ => prev_ident = None,
        }
    }
}

/// Derive that references every component named in `#[route(..)]` /
/// `#[layout(..)]` helper attributes, as the real router expansion does.
#[proc_macro_derive(Routable, attributes(route, layout))]
pub fn routable(item: TokenStream) -> TokenStream {
    let mut refs = String::new();
    collect_route_components(item, &mut refs);
    format!("const _: () = {{ {refs} }};")
        .parse()
        .expect("routable stub expansion parses")
}

/// Reference every routed component, as the real router expansion does: the
/// uppercase idents inside `#[layout(..)]` / `#[route(..)]` attribute args
/// (layout components, explicit component args) and — the common shape — the
/// variant name FOLLOWING a `#[route(..)]` attribute (the router names the
/// component after the variant).
fn collect_route_components(ts: TokenStream, refs: &mut String) {
    let mut prev_ident: Option<String> = None;
    let mut routed_variant = false;
    for tt in ts {
        match tt {
            TokenTree::Ident(i) => {
                let name = i.to_string();
                if routed_variant {
                    refs.push_str(&format!("let _ = {name}; "));
                    routed_variant = false;
                }
                prev_ident = Some(name);
            }
            TokenTree::Group(g) => {
                match prev_ident.take().as_deref() {
                    Some("route") | Some("layout") => {
                        for inner in g.stream() {
                            if let TokenTree::Ident(i) = inner {
                                let name = i.to_string();
                                if name.chars().next().is_some_and(char::is_uppercase) {
                                    refs.push_str(&format!("let _ = {name}; "));
                                }
                            }
                        }
                    }
                    _ => {
                        if g.delimiter() == Delimiter::Bracket
                            && matches!(
                                g.stream().into_iter().next(),
                                Some(TokenTree::Ident(h)) if h.to_string() == "route"
                            )
                        {
                            routed_variant = true;
                        }
                        collect_route_components(g.stream(), refs);
                    }
                }
            }
            _ => prev_ident = None,
        }
    }
}
