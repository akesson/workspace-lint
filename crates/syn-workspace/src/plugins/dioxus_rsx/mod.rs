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
//! The module-tree walker finds the first [`super::ResolverPlugin`] that
//! [`claims_macro`](super::ResolverPlugin::claims_macro)s the macro site; this plugin
//! returns [`Lowered::ScanPlus`](super::Lowered::ScanPlus), so the body is token-scanned
//! (the baseline, like `quote!`) AND the AST walk below adds the Component refs
//! the token scanner misses:
//!
//! - single-ident components without imports
//! - interpolated identifiers in `"{x}"` text segments
//! - component paths inside `for` / `if` template bodies
//!
//! Malformed `rsx!` bodies return an empty reference list; the rustc /
//! dx toolchain is responsible for reporting parse failures, not us.

use std::collections::HashMap;

use syn::parse2;
use syn::visit::Visit;

use crate::plugins::{
    ContributedRef, Fact, LowerCtx, Lowered, MacroSite, Provenance, ResolverPlugin,
};
use crate::resolve::{Crate, ItemKind, Occurrence, Origin, ResolvedPath};

mod routable;
use routable::route_component_occurrences;

/// Built-in lowerer for `rsx!` and `dioxus::rsx!` invocations. Token-scans the
/// body (like any macro) AND adds the structured Component paths the scanner
/// misses — i.e. [`Lowered::ScanPlus`].
pub(crate) struct DioxusPlugin;

impl ResolverPlugin for DioxusPlugin {
    fn claims_macro(&self, site: &MacroSite) -> bool {
        if site.is_macro_rules {
            return false;
        }
        match site.path_segments().segments() {
            [single] => single == "rsx",
            [a, b] => b == "rsx" && (a == "dioxus" || a == "dioxus_core"),
            _ => false,
        }
    }

    /// `rsx!` lowering returns [`Lowered::ScanPlus`], so the per-item nested-body walk
    /// must run for this plugin to reach fn-body `rsx!` invocations.
    fn emits_structured_occurrences(&self) -> bool {
        true
    }

    fn lower_macro(&self, site: &MacroSite, cx: &LowerCtx) -> Lowered {
        let mut collected = Collected::default();
        // Malformed rsx! bodies aren't our problem — the rustc / dx toolchain
        // surfaces those; we just contribute no structured refs from a partial
        // parse (the baseline token scan still runs).
        if let Ok(call_body) = parse2::<dioxus_rsx::CallBody>(site.tokens.clone()) {
            visit_template_body(&call_body.body, &mut collected);
        }
        // Structured refs carry the macro-invocation span (the rsx AST doesn't
        // expose per-ref spans). Qualified paths resolve through the central
        // resolver (`Macro`); a *bare* component name can't be resolved without
        // the whole-workspace component set, so it's `Component` — left for the
        // Phase B `global_facts` hook below.
        let mut occurrences: Vec<Occurrence> = collected
            .macro_paths
            .into_iter()
            .map(|p| Occurrence {
                segments: p.segments().to_vec(),
                path: None,
                span: cx.macro_span.clone(),
                origin: Origin::Macro,
            })
            .collect();
        occurrences.extend(
            collected
                .component_names
                .into_iter()
                .map(|name| Occurrence {
                    segments: vec![name],
                    path: None,
                    span: cx.macro_span.clone(),
                    origin: Origin::Component,
                }),
        );
        Lowered::ScanPlus(occurrences)
    }

    /// Phase A: capture the component references a `#[derive(Routable)]` route enum
    /// makes only through derive-generated code — invisible to the token/AST scans — as
    /// bare [`Origin::Component`] occurrences. [`global_facts`](Self::global_facts) binds
    /// each to the same-crate `pub fn`, exactly like a bare `rsx!` component. A no-op for
    /// any item that isn't a `#[derive(Routable)]` enum.
    fn local_occurrences(&self, item: &syn::Item, file: &std::path::Path) -> Vec<Occurrence> {
        match item {
            syn::Item::Enum(item_enum) => route_component_occurrences(item_enum, file),
            _ => Vec::new(),
        }
    }

    /// Phase B: bind a bare `Foo {}` component invocation inside `rsx!` (or a
    /// `#[derive(Routable)]` route variant) to the matching `pub fn Foo` in the
    /// *same* crate. Bare usages arrive as [`Origin::Component`] occurrences — emitted
    /// by `lower_macro` above and by [`route_component_occurrences`] — so this reads
    /// the resolved model only, no source re-parse.
    ///
    /// Same-crate only: a bare name matches `pub fn`s in its own crate (components are
    /// capitalized `pub fn`s; by-name matching needs no attribute model and only ever
    /// *suppresses* an unused-finding — the FP-safe direction). Cross-crate component
    /// libraries are a documented non-goal; a `use other::Foo;` already counts as a
    /// reference.
    fn global_facts(&self, crates: &[Crate]) -> Vec<Fact> {
        let mut out = Vec::new();
        for krate in crates {
            if !krate.is_workspace_member {
                continue;
            }
            // Candidate component definitions: every public fn, keyed by bare name
            // (a name may be defined in more than one module).
            let mut defs: HashMap<&str, Vec<&ResolvedPath>> = HashMap::new();
            for item in krate.pub_items() {
                if item.kind == ItemKind::Fn {
                    defs.entry(item.name.as_str())
                        .or_default()
                        .push(&item.canonical);
                }
            }
            if defs.is_empty() {
                continue;
            }
            let from = krate.code_name();
            // Bare component usages captured as Origin::Component occurrences.
            for module in krate.all_modules() {
                for occ in &module.occurrences {
                    if occ.origin != Origin::Component {
                        continue;
                    }
                    let Some(name) = occ.segments.last() else {
                        continue;
                    };
                    if let Some(canonicals) = defs.get(name.as_str()) {
                        for canonical in canonicals {
                            out.push(ContributedRef {
                                from: from.clone(),
                                to: (*canonical).clone(),
                                // Component usage sites are rsx!/route captures;
                                // target provenance isn't tracked.
                                via_sibling_target: false,
                            });
                        }
                    }
                }
            }
        }
        out.into_iter()
            .map(|edge| Fact::Reference {
                edge,
                by: Provenance {
                    plugin: "dioxus",
                    rule: "component",
                    trigger: None,
                },
            })
            .collect()
    }
}

/// Structured references gathered from an `rsx!` body, split by how the resolver
/// handles them: multi-segment paths the central resolver canonicalizes vs. bare
/// single-ident component invocations a Phase B pass binds.
#[derive(Default)]
struct Collected {
    macro_paths: Vec<ResolvedPath>,
    component_names: Vec<String>,
}

fn push_component_name(path: &syn::Path, out: &mut Collected) {
    if path.segments.len() >= 2 {
        let segments: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
        out.macro_paths.push(ResolvedPath::new(segments));
    } else if let Some(seg) = path.segments.last() {
        out.component_names.push(seg.ident.to_string());
    }
}

fn visit_template_body(body: &dioxus_rsx::TemplateBody, out: &mut Collected) {
    for node in &body.roots {
        visit_node(node, out);
    }
}

fn visit_node(node: &dioxus_rsx::BodyNode, out: &mut Collected) {
    match node {
        dioxus_rsx::BodyNode::Component(component) => {
            push_component_name(&component.name, out);
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

fn visit_if_chain(chain: &dioxus_rsx::IfChain, out: &mut Collected) {
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
    out: &'a mut Collected,
}

impl<'ast, 'a> Visit<'ast> for ExprPathVisitor<'a> {
    fn visit_path(&mut self, node: &'ast syn::Path) {
        // Interpolated expressions contribute qualified paths only; bare idents
        // are local vars, deliberately dropped.
        if let Some(path) = path_to_resolved(node) {
            self.out.macro_paths.push(path);
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
        DioxusPlugin.claims_macro(&MacroSite {
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
        match DioxusPlugin.lower_macro(&site, &LowerCtx { macro_span: None }) {
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

    // --- bare component capture (Origin::Component) ---

    fn lowered(body: TokenStream) -> Vec<Occurrence> {
        let p = syn::parse_str("rsx").expect("valid path");
        let site = MacroSite {
            is_macro_rules: false,
            path: &p,
            tokens: &body,
            marker_crates: &[],
        };
        match DioxusPlugin.lower_macro(&site, &LowerCtx { macro_span: None }) {
            Lowered::ScanPlus(occs) => occs,
            _ => panic!("rsx lowerer should ScanPlus"),
        }
    }

    #[test]
    fn bare_component_is_component_origin() {
        let occs = lowered(quote! { Card {} });
        let bare: Vec<_> = occs
            .iter()
            .filter(|o| o.origin == Origin::Component)
            .collect();
        assert_eq!(bare.len(), 1, "got {occs:?}");
        assert_eq!(bare[0].segments, vec!["Card".to_string()]);
        assert!(bare[0].path.is_none());
    }

    #[test]
    fn qualified_component_is_macro_origin_not_component() {
        let occs = lowered(quote! { crate::ui::Button {} });
        assert!(
            occs.iter().all(|o| o.origin != Origin::Component),
            "{occs:?}"
        );
        assert!(
            occs.iter()
                .any(|o| o.origin == Origin::Macro && o.segments == ["crate", "ui", "Button"])
        );
    }
}
