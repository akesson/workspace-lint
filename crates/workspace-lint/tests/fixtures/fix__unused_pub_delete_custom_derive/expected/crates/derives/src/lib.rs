use proc_macro::{TokenStream, TokenTree};

/// `impl <Type> { pub fn stringify(&self) -> &'static str }` — the
/// `EnumStringify` shape: an inherent impl of the annotated type.
#[proc_macro_derive(Stringify)]
pub fn stringify(item: TokenStream) -> TokenStream {
    let name = adt_name(item);
    format!(
        "impl {name} {{ pub fn stringify(&self) -> &'static str {{ \"{name}\" }} }}"
    )
    .parse()
    .unwrap()
}

/// The identifier after `struct` / `enum` / `union` (no `syn` dep).
fn adt_name(item: TokenStream) -> String {
    let mut after_kw = false;
    for tt in item {
        if let TokenTree::Ident(id) = tt {
            let s = id.to_string();
            if after_kw {
                return s;
            }
            after_kw = matches!(s.as_str(), "struct" | "enum" | "union");
        }
    }
    panic!("derive input has no type name")
}
