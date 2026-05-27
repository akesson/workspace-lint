//! Macro-body parser plugins.
//!
//! Some macros — `rsx!`, `quote!`, `serde_json::json!` — encode meaningful
//! references inside their bodies as full Rust syntax. Token-level scanning
//! (Layer 1 autodetect) often misses these or misclassifies them. Plugins
//! parse such bodies into structured ASTs and emit precise reference lists.
//!
//! ## v1 status — trait surface, no dispatch
//!
//! **The plugin registry is not yet wired into the resolver.** In v1 the
//! built-in [`QuoteParser`] is matched directly inside
//! [`crate::resolve::module_tree::matches_known_plugin_macro`] and its body
//! extracted by the same token scanner Layer 1 uses (since `quote!` bodies
//! degrade gracefully to that). The [`MacroBodyParser::references`] method
//! on the shipped parser therefore returns an empty vec — calling it does
//! not yield references. The trait exists so that:
//!
//! - the public shape of plugin contributions is fixed before v2 (when
//!   parsers that need real AST walks — dioxus-rsx, serde-json — land), and
//! - downstream code can implement and register parsers ahead of the
//!   internal dispatch wiring.
//!
//! Do not rely on [`builtin_parsers`] for extraction in v1; consult the
//! resolver-attached implicit-refs set via [`crate::Workspace::macro_implicit_refs`]
//! instead.
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

pub mod dioxus_rsx;

pub use dioxus_rsx::DioxusRsxParser;

/// Context passed to a plugin while it's resolving references inside a
/// macro body.
///
/// Currently a placeholder; will carry the resolved workspace, the current
/// crate, and the surrounding scope so parsers can resolve identifiers in
/// the macro body the same way Tier 1 resolves them in regular code.
pub struct ResolveContext<'a> {
    _phantom: std::marker::PhantomData<&'a ()>,
}

impl ResolveContext<'_> {
    /// V1 stub context. Plugins that need scope/use-bindings to canonicalize
    /// single-segment paths won't be able to with this — they must emit
    /// multi-segment refs that the caller resolves through
    /// [`crate::resolve::module_tree`]'s scope rules. The full context is
    /// added in v2.
    pub(crate) fn placeholder() -> Self {
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
/// Ships two parsers: [`QuoteParser`] for `quote!`/`quote::quote!` and
/// [`DioxusRsxParser`] for `rsx!`/`dioxus::rsx!`. A future `serde_json::json!`
/// parser is on the roadmap but considered speculative — the references it
/// would surface are almost always already visible elsewhere in the file.
pub fn builtin_parsers() -> Vec<Box<dyn MacroBodyParser>> {
    vec![Box::new(QuoteParser), Box::new(DioxusRsxParser)]
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
        // v1 stub: extraction lives in the module-tree walker
        // (`extract_macro_paths`), which is what the resolver actually calls
        // when it encounters a `quote!` invocation. Returning empty here is
        // deliberate — see the module doc. The trait method body will be
        // filled in when the resolver starts dispatching through
        // `builtin_parsers()` in v2.
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
