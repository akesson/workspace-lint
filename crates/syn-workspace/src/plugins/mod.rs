//! Macro-body parser plugins (internal).
//!
//! Some macros — `rsx!`, `quote!`, `serde_json::json!` — encode meaningful
//! references inside their bodies as full Rust syntax. Token-level scanning
//! (Layer 1 autodetect) often misses these or misclassifies them. Plugins
//! parse such bodies into structured ASTs and emit precise reference lists.
//!
//! **This is an internal extension point**, not a public API. The trait,
//! the context type, and the built-in parser list are all `pub(crate)` and
//! exist so that built-in parsers (currently [`QuoteParser`] and
//! [`DioxusRsxParser`]) can be added in one place without spreading their
//! match logic across the resolver. Downstream consumers do not implement
//! this trait; they consume the resolved references via
//! [`crate::Workspace`].
//!
//! ## Adding a built-in parser
//!
//! 1. Implement [`MacroBodyParser`] in a new module under `plugins/`.
//! 2. Append a `Box::new(MyParser)` entry to [`builtin_parsers`].
//! 3. Add fixtures under
//!    `crates/workspace-lint/tests/cases/<lint>/{true_negatives,known_false_positives}/`.

use proc_macro2::TokenStream;

use crate::resolve::ResolvedPath;

pub(crate) mod dioxus_rsx;

pub(crate) use dioxus_rsx::DioxusRsxParser;

/// Context passed to a plugin while it's resolving references inside a
/// macro body. Currently carries no state; the type exists so the trait
/// method's signature is stable when scope-aware resolution lands.
pub(crate) struct ResolveContext<'a> {
    _phantom: std::marker::PhantomData<&'a ()>,
}

impl ResolveContext<'_> {
    pub(crate) fn placeholder() -> Self {
        Self {
            _phantom: std::marker::PhantomData,
        }
    }
}

/// A pluggable parser for specific macro bodies. Plugins extend the resolver
/// with structured knowledge of macros whose contents are richer than raw
/// token streams.
pub(crate) trait MacroBodyParser: Send + Sync {
    /// Returns `true` if this parser knows the macro at `macro_path`.
    ///
    /// Implementations should match both unqualified and crate-qualified
    /// forms (e.g. `rsx!` and `dioxus::rsx!`) since the call site may use
    /// either depending on its `use` statements.
    fn matches(&self, macro_path: &ResolvedPath) -> bool;

    /// Extract references from the macro's body. Implementations that rely
    /// solely on Layer 1 token-scanning (currently [`QuoteParser`]) return
    /// an empty vec — they exist only to gate Layer 1 via [`Self::matches`].
    fn references(&self, body: &TokenStream, cx: &ResolveContext<'_>) -> Vec<ResolvedPath>;
}

/// All built-in plugin parsers shipped with `syn-workspace`.
pub(crate) fn builtin_parsers() -> Vec<Box<dyn MacroBodyParser>> {
    vec![Box::new(QuoteParser), Box::new(DioxusRsxParser)]
}

/// Built-in parser for `quote!` / `quote::quote!` invocations.
///
/// `quote!` bodies are token streams the proc-macro emits as Rust source at
/// expansion time. Their contents reference items the caller will see at
/// the expansion site; Layer 1 token scanning (extract multi-segment path
/// tokens, resolve through the call-site scope) handles them. This parser
/// only contributes the `matches` predicate that gates the token scan —
/// `references` is intentionally empty.
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

    struct AlwaysMatches;

    impl MacroBodyParser for AlwaysMatches {
        fn matches(&self, _macro_path: &ResolvedPath) -> bool {
            true
        }

        fn references(&self, _body: &TokenStream, _cx: &ResolveContext<'_>) -> Vec<ResolvedPath> {
            vec![ResolvedPath::new(["dummy", "Thing"])]
        }
    }

    #[test]
    fn trait_is_object_safe() {
        let parsers: Vec<Box<dyn MacroBodyParser>> = vec![Box::new(AlwaysMatches)];
        let path = ResolvedPath::new(["foo", "bar"]);
        let cx = ResolveContext::placeholder();
        let tokens = TokenStream::new();
        for p in &parsers {
            assert!(p.matches(&path));
            assert_eq!(p.references(&tokens, &cx).len(), 1);
        }
    }

    #[test]
    fn builtin_parsers_includes_quote() {
        let parsers = builtin_parsers();
        assert!(!parsers.is_empty(), "ships at least the quote parser");
        let quote_path = ResolvedPath::new(["quote"]);
        let quote_qualified = ResolvedPath::new(["quote", "quote"]);
        assert!(parsers.iter().any(|p| p.matches(&quote_path)));
        assert!(parsers.iter().any(|p| p.matches(&quote_qualified)));
    }

    #[test]
    fn quote_parser_does_not_match_unrelated_macros() {
        let parsers = builtin_parsers();
        let lazy_static = ResolvedPath::new(["lazy_static"]);
        assert!(!parsers.iter().any(|p| p.matches(&lazy_static)));
    }

    #[test]
    fn builtin_parsers_includes_dioxus_rsx() {
        let parsers = builtin_parsers();
        let rsx = ResolvedPath::new(["rsx"]);
        let dioxus_rsx_qualified = ResolvedPath::new(["dioxus", "rsx"]);
        assert!(parsers.iter().any(|p| p.matches(&rsx)));
        assert!(parsers.iter().any(|p| p.matches(&dioxus_rsx_qualified)));
    }
}
