//! Macro-body parser plugins (internal).
//!
//! Some macros — `rsx!`, `quote!`, `serde_json::json!` — encode meaningful
//! references inside their bodies as full Rust syntax. Token-level scanning
//! (Layer 1 autodetect) often misses these or misclassifies them. Plugins
//! parse such bodies into structured ASTs and emit precise reference lists.
//!
//! **This is an internal extension point**, not a public API. The trait,
//! the context type, the registry, and the [`matches()`]/[`refs()`] dispatch
//! functions are all `pub(crate)` and exist so that built-in parsers can be
//! added in one place. Downstream consumers do not implement this trait;
//! they consume the resolved references via [`crate::Workspace`].
//!
//! ## Adding a built-in parser
//!
//! Each plugin lives in its own folder. To add one:
//!
//! 1. Create `plugins/<name>/mod.rs`. Define your parser struct and
//!    `impl MacroBodyParser for ...`. Colocate unit tests in a
//!    `#[cfg(test)] mod tests` block in the same file.
//! 2. **If your parser brings an extra crate dep**: mark that dep
//!    `optional = true` in `crates/syn-workspace/Cargo.toml`, add a
//!    `<name>` entry to the `[features]` table (and to `default` if it
//!    should ship enabled by default), and gate the `mod <name>;` line
//!    below with `#[cfg(feature = "<name>")]`.
//! 3. Append `Box::new(<name>::Parser)` to [`builtin_parsers`] (with the
//!    same `#[cfg]` gate if your parser is feature-flagged).
//! 4. Add a `builtin_parsers_includes_<name>` test in the registry tests
//!    below (cfg-gated to match).
//!
//! There is no separate gating table to update — the module-tree walker
//! goes through [`matches()`], which iterates [`builtin_parsers`] and asks
//! each parser whether it claims the macro path.

use proc_macro2::TokenStream;

use crate::resolve::ResolvedPath;

pub(crate) mod quote;

#[cfg(feature = "dioxus")]
pub(crate) mod dioxus_rsx;

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
    /// solely on Layer 1 token-scanning (currently [`quote::QuoteParser`])
    /// return an empty vec — they exist only to gate Layer 1 via
    /// [`Self::matches`].
    fn references(&self, body: &TokenStream, cx: &ResolveContext<'_>) -> Vec<ResolvedPath>;
}

/// All built-in plugin parsers shipped with `syn-workspace`.
pub(crate) fn builtin_parsers() -> Vec<Box<dyn MacroBodyParser>> {
    // `mut` only needed when at least one feature-gated parser is enabled.
    #[allow(unused_mut)]
    let mut v: Vec<Box<dyn MacroBodyParser>> = vec![Box::new(quote::QuoteParser)];
    #[cfg(feature = "dioxus")]
    v.push(Box::new(dioxus_rsx::DioxusRsxParser));
    v
}

/// Returns `true` if any built-in plugin parser claims the macro at `path`.
///
/// Single source of truth for the "is this a known plugin macro?" gate
/// used by the module-tree walker — both to enable Layer 1 token scanning
/// of bodies and to suppress double-counting in the implicit-refs filter.
/// The matched path set is derived from each parser's `matches()`, so
/// adding a plugin does not require any edits here.
pub(crate) fn matches(path: &syn::Path) -> bool {
    let segs: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
    let rp = ResolvedPath::new(segs);
    builtin_parsers().iter().any(|p| p.matches(&rp))
}

/// Dispatch a macro invocation to the built-in plugin registry and collect
/// any references the matching parser emits. Returned paths are in raw
/// (uncanonicalized) form — the caller runs them through
/// [`crate::resolve::module_tree::resolve_macro_path`] to apply scope rules.
pub(crate) fn refs(macro_path: &syn::Path, body: &TokenStream) -> Vec<ResolvedPath> {
    let segs: Vec<String> = macro_path
        .segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect();
    let mp = ResolvedPath::new(segs);
    let cx = ResolveContext::placeholder();
    let mut out: Vec<ResolvedPath> = Vec::new();
    for parser in builtin_parsers() {
        if parser.matches(&mp) {
            out.extend(parser.references(body, &cx));
        }
    }
    out
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

    #[cfg(feature = "dioxus")]
    #[test]
    fn builtin_parsers_includes_dioxus_rsx() {
        let parsers = builtin_parsers();
        let rsx = ResolvedPath::new(["rsx"]);
        let dioxus_rsx_qualified = ResolvedPath::new(["dioxus", "rsx"]);
        assert!(parsers.iter().any(|p| p.matches(&rsx)));
        assert!(parsers.iter().any(|p| p.matches(&dioxus_rsx_qualified)));
    }

    fn syn_path(s: &str) -> syn::Path {
        syn::parse_str(s).expect("valid path")
    }

    #[test]
    fn matches_gates_known_plugin_macros() {
        assert!(matches(&syn_path("quote")));
        assert!(matches(&syn_path("quote::quote")));
    }

    #[cfg(feature = "dioxus")]
    #[test]
    fn matches_gates_rsx_when_dioxus_enabled() {
        assert!(matches(&syn_path("rsx")));
        assert!(matches(&syn_path("dioxus::rsx")));
    }

    #[test]
    fn matches_rejects_unrelated_macros() {
        assert!(!matches(&syn_path("lazy_static")));
        assert!(!matches(&syn_path("serde_json::json")));
    }
}
