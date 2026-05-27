//! Macro-body parser plugins.
//!
//! Some macros — `rsx!`, `quote!`, `serde_json::json!` — encode meaningful
//! references inside their bodies as full Rust syntax. Token-level scanning
//! (Layer 1 autodetect) often misses these or misclassifies them. Plugins
//! parse such bodies into structured ASTs and emit precise reference lists.
//!
//! ## Always built-in
//!
//! Plugins ship as modules inside this crate and are unconditionally
//! compiled. There are no Cargo features for opting in or out — prebuilt
//! binaries absorb the compile cost so users get them for free.
//!
//! Add a new plugin in three steps:
//!
//! 1. Implement [`MacroBodyParser`] in a new module under `plugins/`.
//! 2. Append a `Box::new(MyParser)` entry to [`builtin_parsers`].
//! 3. Add fixtures under
//!    `crates/workspace-lint/tests/cases/<lint>/{true_negatives,known_false_positives}/`.
//!
//! ## Third-party plugins
//!
//! Downstream crates can implement the trait too and register at runtime via
//! a future `Workspace::register_parser` API. The canonical path for popular
//! macros, however, is to upstream the parser into `plugins/`.

use proc_macro2::TokenStream;

use crate::resolve::ResolvedPath;

/// Context passed to a plugin while it's resolving references inside a
/// macro body.
///
/// Currently a placeholder; will carry the resolved workspace, the current
/// crate, and the surrounding scope so parsers can resolve identifiers in
/// the macro body the same way Tier 1 resolves them in regular code.
pub struct ResolveContext<'a> {
    _phantom: std::marker::PhantomData<&'a ()>,
}

#[cfg(test)]
impl ResolveContext<'_> {
    fn placeholder() -> Self {
        Self {
            _phantom: std::marker::PhantomData,
        }
    }
}

/// A pluggable parser for specific macro bodies. Plugins extend the resolver
/// with structured knowledge of macros whose contents are richer than raw
/// token streams.
pub trait MacroBodyParser: Send + Sync {
    /// Returns `true` if this parser knows the macro at `macro_path`.
    ///
    /// Implementations should match both unqualified and crate-qualified
    /// forms (e.g. `rsx!` and `dioxus::rsx!`) since the call site may use
    /// either depending on its `use` statements.
    fn matches(&self, macro_path: &ResolvedPath) -> bool;

    /// Extract references from the macro's body.
    fn references(&self, body: &TokenStream, cx: &ResolveContext<'_>) -> Vec<ResolvedPath>;
}

/// All plugins shipped inside `syn-workspace`.
///
/// v1 ships one parser: [`QuoteParser`] for `quote!` / `quote::quote!`
/// invocations. `dioxus_rsx` and `serde_json::json!` are planned follow-ups
/// (dioxus-rsx needs an external dep pinned to a specific Dioxus version;
/// serde_json::json! has lower value and is mostly speculative).
pub fn builtin_parsers() -> Vec<Box<dyn MacroBodyParser>> {
    vec![Box::new(QuoteParser)]
}

/// Built-in parser for `quote!` / `quote::quote!` invocations.
///
/// `quote!` bodies are token streams that the proc-macro will emit as Rust
/// source at expansion time. Their contents reference items the caller
/// will see at the expansion site; treating them like `macro_rules!` bodies
/// (extract multi-segment path tokens, resolve through the call-site
/// scope) is a clean reuse of the Layer 1 infrastructure.
pub struct QuoteParser;

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

    fn references(&self, body: &TokenStream, _cx: &ResolveContext<'_>) -> Vec<ResolvedPath> {
        // The token-scanning code lives in the module-tree walker; this
        // method exists for the trait surface but the call site passes
        // the already-extracted paths back into the resolver pipeline.
        // For external callers who want to use the trait outside the
        // resolver, an in-method extraction would go here.
        let _ = body;
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
        assert!(!parsers.is_empty(), "v1 ships at least the quote parser");
        let quote_path = ResolvedPath::new(["quote"]);
        let quote_qualified = ResolvedPath::new(["quote", "quote"]);
        assert!(parsers.iter().any(|p| p.matches(&quote_path)));
        assert!(parsers.iter().any(|p| p.matches(&quote_qualified)));
    }

    #[test]
    fn quote_parser_does_not_match_unrelated_macros() {
        let parsers = builtin_parsers();
        let lazy_static = ResolvedPath::new(["lazy_static"]);
        let rsx = ResolvedPath::new(["dioxus", "rsx"]);
        assert!(!parsers.iter().any(|p| p.matches(&lazy_static)));
        assert!(!parsers.iter().any(|p| p.matches(&rsx)));
    }
}
