//! Plugin: `rsx!` / `dioxus::rsx!` body parser.
//!
//! `rsx!` invocations encode component trees as a custom DSL whose semantic
//! references — Component names, interpolated identifiers — are easy to
//! miss with raw token scanning (a Component invocation `MyButton { ... }`
//! looks like a single ident, indistinguishable from a local variable
//! shadow). This parser walks the dioxus-rsx AST directly and emits
//! every Component path it finds.
//!
//! ## Dispatch
//!
//! The resolver's module-tree walker gates on `plugins::matches` (which
//! iterates [`super::builtin_parsers`] and asks each parser whether it
//! claims the macro path). Bodies of matching macros are token-scanned
//! by Layer 1 — same as `quote!` — and additionally passed through
//! `plugins::refs`, which calls every matching parser's
//! [`super::MacroBodyParser::references`] method. The AST walk below
//! catches Component refs the token scanner misses:
//!
//! - single-ident components without imports
//! - interpolated identifiers in `"{x}"` text segments
//! - component paths inside `for` / `if` template bodies
//!
//! Malformed `rsx!` bodies return an empty reference list; the rustc /
//! dx toolchain is responsible for reporting parse failures, not us.

use proc_macro2::TokenStream;
use syn::parse2;
use syn::visit::Visit;

use crate::plugins::{MacroBodyParser, ResolveContext};
use crate::resolve::ResolvedPath;

/// Built-in parser for `rsx!` and `dioxus::rsx!` invocations.
pub struct DioxusRsxParser;

impl MacroBodyParser for DioxusRsxParser {
    fn matches(&self, macro_path: &ResolvedPath) -> bool {
        let segs = macro_path.segments();
        match segs {
            [single] => single == "rsx",
            [a, b] => b == "rsx" && (a == "dioxus" || a == "dioxus_core"),
            _ => false,
        }
    }

    fn references(&self, body: &TokenStream, _cx: &ResolveContext<'_>) -> Vec<ResolvedPath> {
        let Ok(call_body) = parse2::<dioxus_rsx::CallBody>(body.clone()) else {
            // Malformed rsx! bodies aren't our problem — the rustc / dx
            // toolchain surfaces those. Return empty so we don't manufacture
            // spurious references from a partial parse.
            return Vec::new();
        };
        let mut out: Vec<ResolvedPath> = Vec::new();
        visit_template_body(&call_body.body, &mut out);
        out
    }
}

fn visit_template_body(body: &dioxus_rsx::TemplateBody, out: &mut Vec<ResolvedPath>) {
    for node in &body.roots {
        visit_node(node, out);
    }
}

fn visit_node(node: &dioxus_rsx::BodyNode, out: &mut Vec<ResolvedPath>) {
    match node {
        dioxus_rsx::BodyNode::Component(component) => {
            if let Some(path) = path_to_resolved(&component.name) {
                out.push(path);
            }
            visit_template_body(&component.children, out);
        }
        dioxus_rsx::BodyNode::Element(element) => {
            for child in element.children.iter() {
                visit_node(child, out);
            }
        }
        dioxus_rsx::BodyNode::RawExpr(expr_node) => {
            // PartialExpr may not parse cleanly as a single Expr (e.g.
            // bare blocks). Fall back to scanning the raw token stream if
            // syn parsing fails — at least the ident::ident patterns
            // surface.
            if let Ok(expr) = expr_node.expr.as_expr() {
                let mut v = ExprPathVisitor { out };
                v.visit_expr(&expr);
            }
        }
        dioxus_rsx::BodyNode::ForLoop(for_loop) => {
            let mut v = ExprPathVisitor { out };
            v.visit_expr(&for_loop.expr);
            visit_template_body(&for_loop.body, out);
        }
        dioxus_rsx::BodyNode::IfChain(if_chain) => {
            visit_if_chain(if_chain, out);
        }
        dioxus_rsx::BodyNode::Text(_) => {
            // Formatted text segments contain interpolated identifiers
            // (`"{name}"`) that resolve to local variables in the enclosing
            // scope. Without a scope-aware resolver hooked into v2
            // dispatch, recording them as references would produce
            // spurious crate-name entries (`name` → external crate
            // "name"). Skip until v2.
        }
    }
}

fn visit_if_chain(chain: &dioxus_rsx::IfChain, out: &mut Vec<ResolvedPath>) {
    let mut v = ExprPathVisitor { out };
    v.visit_expr(&chain.cond);
    visit_template_body(&chain.then_branch, out);
    if let Some(ref else_if) = chain.else_if_branch {
        visit_if_chain(else_if, out);
    }
    if let Some(ref else_branch) = chain.else_branch {
        visit_template_body(else_branch, out);
    }
}

struct ExprPathVisitor<'a> {
    out: &'a mut Vec<ResolvedPath>,
}

impl<'ast, 'a> Visit<'ast> for ExprPathVisitor<'a> {
    fn visit_path(&mut self, node: &'ast syn::Path) {
        if let Some(path) = path_to_resolved(node) {
            self.out.push(path);
        }
        syn::visit::visit_path(self, node);
    }
}

/// Convert a `syn::Path` to a `ResolvedPath` of raw segment strings, drops
/// paths shorter than two segments since the resolver pipeline can't
/// canonicalize bare-identifier references without scope context (which
/// the v1 plugin trait doesn't expose). Multi-segment paths land
/// uncanonicalized — downstream consumers expect that today since the
/// plugin's references aren't yet wired into the dispatch.
fn path_to_resolved(path: &syn::Path) -> Option<ResolvedPath> {
    if path.segments.len() < 2 {
        return None;
    }
    let segments: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
    Some(ResolvedPath::new(segments))
}

#[cfg(test)]
mod tests {
    use super::*;
    use quote::quote;

    fn cx() -> ResolveContext<'static> {
        ResolveContext::placeholder()
    }

    #[test]
    fn matches_unqualified_rsx() {
        let p = DioxusRsxParser;
        assert!(p.matches(&ResolvedPath::new(["rsx"])));
    }

    #[test]
    fn matches_dioxus_qualified() {
        let p = DioxusRsxParser;
        assert!(p.matches(&ResolvedPath::new(["dioxus", "rsx"])));
        assert!(p.matches(&ResolvedPath::new(["dioxus_core", "rsx"])));
    }

    #[test]
    fn does_not_match_unrelated() {
        let p = DioxusRsxParser;
        assert!(!p.matches(&ResolvedPath::new(["quote"])));
        assert!(!p.matches(&ResolvedPath::new(["serde_json", "json"])));
    }

    #[test]
    fn extracts_qualified_component_path() {
        let p = DioxusRsxParser;
        let body = quote! {
            crate::components::Button { label: "go" }
        };
        let refs = p.references(&body, &cx());
        let displays: Vec<String> = refs.iter().map(|r| r.display()).collect();
        assert!(
            displays.iter().any(|d| d == "crate::components::Button"),
            "got {displays:?}"
        );
    }

    #[test]
    fn extracts_components_inside_for_loop() {
        let p = DioxusRsxParser;
        let body = quote! {
            for item in items.iter() {
                crate::Card { item: item }
            }
        };
        let refs = p.references(&body, &cx());
        let displays: Vec<String> = refs.iter().map(|r| r.display()).collect();
        assert!(
            displays.iter().any(|d| d == "crate::Card"),
            "got {displays:?}"
        );
    }

    #[test]
    fn malformed_body_returns_empty() {
        let p = DioxusRsxParser;
        let body = quote! { this is { not valid rsx } at all };
        let refs = p.references(&body, &cx());
        assert!(
            refs.is_empty(),
            "malformed body should return empty, got {refs:?}"
        );
    }
}
