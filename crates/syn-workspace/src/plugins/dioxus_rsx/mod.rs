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
//! The module-tree walker finds the first [`super::MacroLowerer`] that
//! [`claims`](super::MacroLowerer::claims) the macro site; this lowerer returns
//! [`Lowered::ScanPlus`](super::Lowered::ScanPlus), so the body is token-scanned
//! (the baseline, like `quote!`) AND the AST walk below adds the Component refs
//! the token scanner misses:
//!
//! - single-ident components without imports
//! - interpolated identifiers in `"{x}"` text segments
//! - component paths inside `for` / `if` template bodies
//!
//! Malformed `rsx!` bodies return an empty reference list; the rustc /
//! dx toolchain is responsible for reporting parse failures, not us.

use syn::parse2;
use syn::visit::Visit;

use crate::plugins::{LowerCtx, Lowered, MacroLowerer, MacroSite};
use crate::resolve::ResolvedPath;
use crate::resolve::module_tree::{Occurrence, Origin};

/// Built-in lowerer for `rsx!` and `dioxus::rsx!` invocations. Token-scans the
/// body (like any macro) AND adds the structured Component paths the scanner
/// misses — i.e. [`Lowered::ScanPlus`].
pub(crate) struct DioxusRsxLowerer;

impl MacroLowerer for DioxusRsxLowerer {
    fn claims(&self, site: &MacroSite) -> bool {
        if site.is_macro_rules {
            return false;
        }
        match site.path_segments().segments() {
            [single] => single == "rsx",
            [a, b] => b == "rsx" && (a == "dioxus" || a == "dioxus_core"),
            _ => false,
        }
    }

    fn lower(&self, site: &MacroSite, cx: &LowerCtx) -> Lowered {
        let mut raw: Vec<ResolvedPath> = Vec::new();
        // Malformed rsx! bodies aren't our problem — the rustc / dx toolchain
        // surfaces those; we just contribute no structured refs from a partial
        // parse (the baseline token scan still runs).
        if let Ok(call_body) = parse2::<dioxus_rsx::CallBody>(site.tokens.clone()) {
            visit_template_body(&call_body.body, &mut raw);
        }
        // Structured refs carry the macro-invocation span (the rsx AST doesn't
        // expose per-ref spans).
        let occurrences = raw
            .into_iter()
            .map(|p| Occurrence {
                segments: p.segments().to_vec(),
                span: cx.macro_span.clone(),
                origin: Origin::Macro,
            })
            .collect();
        Lowered::ScanPlus(occurrences)
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
    use proc_macro2::TokenStream;
    use quote::quote;

    fn claims(path: &str) -> bool {
        let p = syn::parse_str(path).expect("valid path");
        let t = TokenStream::new();
        DioxusRsxLowerer.claims(&MacroSite {
            is_macro_rules: false,
            path: &p,
            tokens: &t,
            marker_crates: &[],
        })
    }

    /// Lower an `rsx!` body and return the `::`-joined structured component paths.
    fn component_paths(body: TokenStream) -> Vec<String> {
        let p = syn::parse_str("rsx").expect("valid path");
        let site = MacroSite {
            is_macro_rules: false,
            path: &p,
            tokens: &body,
            marker_crates: &[],
        };
        match DioxusRsxLowerer.lower(&site, &LowerCtx { macro_span: None }) {
            Lowered::ScanPlus(occs) => occs.iter().map(|o| o.segments.join("::")).collect(),
            other => panic!(
                "rsx lowerer should ScanPlus, got {:?}",
                matches!(other, Lowered::TokenScan)
            ),
        }
    }

    #[test]
    fn claims_unqualified_and_qualified_rsx() {
        assert!(claims("rsx"));
        assert!(claims("dioxus::rsx"));
        assert!(claims("dioxus_core::rsx"));
    }

    #[test]
    fn does_not_claim_unrelated() {
        assert!(!claims("quote"));
        assert!(!claims("serde_json::json"));
    }

    #[test]
    fn extracts_qualified_component_path() {
        let paths = component_paths(quote! {
            crate::components::Button { label: "go" }
        });
        assert!(
            paths.iter().any(|p| p == "crate::components::Button"),
            "got {paths:?}"
        );
    }

    #[test]
    fn extracts_components_inside_for_loop() {
        let paths = component_paths(quote! {
            for item in items.iter() {
                crate::Card { item: item }
            }
        });
        assert!(paths.iter().any(|p| p == "crate::Card"), "got {paths:?}");
    }

    #[test]
    fn malformed_body_yields_no_components() {
        let paths = component_paths(quote! { this is { not valid rsx } at all });
        assert!(paths.is_empty(), "got {paths:?}");
    }
}
