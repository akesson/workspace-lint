//! Stand-in for strum_macros' `EnumString` derive with the real expansion's
//! load-bearing shape: the generated `impl FromStr` references
//! `strum::ParseError`, so the `strum` dep is exercised only through
//! generated code — the premise of the fixture. (The syn backend can't expand
//! and needs its name-keyed assertion; the rustc backend sees the edge
//! natively in post-expansion HIR.)
use proc_macro::TokenStream;
use proc_macro::TokenTree;

#[proc_macro_derive(EnumString)]
pub fn enum_string(item: TokenStream) -> TokenStream {
    // Hand-rolled ident scan (a stub can't depend on `syn`): the token after
    // the `enum` keyword is the type name.
    let mut tokens = item.into_iter();
    let mut name = None;
    while let Some(tt) = tokens.next() {
        if let TokenTree::Ident(id) = tt
            && id.to_string() == "enum"
        {
            if let Some(TokenTree::Ident(id)) = tokens.next() {
                name = Some(id.to_string());
            }
            break;
        }
    }
    let name = name.expect("derive input must be an enum");
    format!(
        "impl core::str::FromStr for {name} {{\n\
             type Err = strum::ParseError;\n\
             fn from_str(_s: &str) -> Result<Self, Self::Err> {{\n\
                 Err(strum::ParseError::VariantNotFound)\n\
             }}\n\
         }}"
    )
    .parse()
    .unwrap()
}
