//! Plugin dispatch — the gateway between the module-tree walker and the
//! built-in [`crate::plugins`] parser registry.
//!
//! When the walker encounters a macro invocation, two things happen:
//!
//! 1. [`matches_known_plugin_macro`] gates Layer 1 token scanning for macro
//!    bodies whose contents reference items the call site needs to see
//!    (`quote!`, `rsx!`, …). Bodies of macros NOT on this list are skipped
//!    entirely — most macros encode local DSL state, not item references.
//! 2. [`dispatch_plugin_refs`] runs every matching built-in parser's
//!    `references()` method to surface refs the token scanner alone would
//!    miss (currently used by `dioxus-rsx`'s AST walker for component
//!    paths).
//!
//! Both layers feed into the same `macro_implicit_refs` set on the module.

use crate::plugins;
use crate::resolve::ResolvedPath;

/// Match invocations of macros that have a built-in
/// [`crate::plugins::MacroBodyParser`] — currently `quote!`/`quote::quote!`
/// and `rsx!`/`dioxus::rsx!`. Every match triggers Layer 1 token scanning
/// of the macro body plus [`dispatch_plugin_refs`] for any parser whose
/// `references()` walks a real AST.
pub(crate) fn matches_known_plugin_macro(path: &syn::Path) -> bool {
    let segs: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
    match segs.as_slice() {
        [single] => single == "quote" || single == "rsx",
        [a, b] => {
            (a == "quote" && b == "quote") || (b == "rsx" && (a == "dioxus" || a == "dioxus_core"))
        }
        _ => false,
    }
}

/// Dispatch a macro invocation to the built-in plugin registry and collect
/// any references the matching parser emits. Returned paths are in raw
/// (uncanonicalized) form — the caller runs them through
/// [`crate::resolve::module_tree::resolve_macro_path`] to apply scope rules.
pub(crate) fn dispatch_plugin_refs(
    macro_path: &syn::Path,
    body: &proc_macro2::TokenStream,
) -> Vec<ResolvedPath> {
    let segs: Vec<String> = macro_path
        .segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect();
    let mp = ResolvedPath::new(segs);
    let cx = plugins::ResolveContext::placeholder();
    let mut out: Vec<ResolvedPath> = Vec::new();
    for parser in plugins::builtin_parsers() {
        if parser.matches(&mp) {
            out.extend(parser.references(body, &cx));
        }
    }
    out
}
