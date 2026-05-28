//! Plugin: `quote!` / `quote::quote!` body parser (gate only).
//!
//! See [`crate::plugins`] for the contributor recipe. `quote!` bodies are
//! token streams the proc-macro emits as Rust source at expansion time;
//! their contents reference items the caller will see at the expansion
//! site, and Layer 1 token scanning (extract multi-segment path tokens,
//! resolve through the call-site scope) handles them. This parser only
//! contributes the [`MacroBodyParser::matches`] predicate that gates the
//! token scan — [`MacroBodyParser::references`] is intentionally empty.

use proc_macro2::TokenStream;

use crate::plugins::{MacroBodyParser, ResolveContext};
use crate::resolve::ResolvedPath;

/// Built-in parser for `quote!` and `quote::quote!` invocations.
pub(crate) struct QuoteParser;

impl MacroBodyParser for QuoteParser {
    fn matches(&self, macro_path: &ResolvedPath) -> bool {
        let segs = macro_path.segments();
        // Match bare `quote!` and `quote::quote!`. Other suffixes
        // (`quote_spanned!`, `format_ident!`) intentionally don't match —
        // their body semantics differ.
        match segs {
            [single] => single == "quote",
            [a, b] => a == "quote" && b == "quote",
            _ => false,
        }
    }

    fn references(&self, _body: &TokenStream, _cx: &ResolveContext<'_>) -> Vec<ResolvedPath> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_bare_quote() {
        let p = QuoteParser;
        assert!(p.matches(&ResolvedPath::new(["quote"])));
    }

    #[test]
    fn matches_quote_qualified() {
        let p = QuoteParser;
        assert!(p.matches(&ResolvedPath::new(["quote", "quote"])));
    }

    #[test]
    fn does_not_match_unrelated_macros() {
        let p = QuoteParser;
        assert!(!p.matches(&ResolvedPath::new(["lazy_static"])));
        assert!(!p.matches(&ResolvedPath::new(["quote_spanned"])));
    }
}
