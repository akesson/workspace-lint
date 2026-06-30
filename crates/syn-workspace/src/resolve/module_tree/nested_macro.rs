//! The nested-macro-body lowering visitor, factored out of the module-tree
//! walk (`mod.rs`) to keep that file focused.

use std::path::Path;

use super::span_to_source_span;
use crate::plugins;
use crate::resolve::Occurrence;

/// Visits an item's nested bodies (fn bodies, expression position, …) and
/// dispatches every claimed macro invocation to a structured lowerer, collecting
/// the structured occurrences it emits. This is how fn-body `rsx!` (the realistic
/// position) reaches the lowerer — the item-position branch in the main walk only
/// sees `syn::Item::Macro`. Only [`plugins::Lowered::ScanPlus`] /
/// [`plugins::Lowered::Exact`] contribute; the baseline token scan
/// (`extract_code_paths`) already covers fn-body macro *tokens*, so a
/// `TokenScan` lowerer would double-count and is skipped here.
///
/// A macro inside a fn-body-nested `mod`/`fn` is attributed to the enclosing
/// module rather than the nested one — harmless for the same-crate lints this
/// feeds, and a documented non-goal.
pub(super) struct NestedMacroLowering<'a> {
    pub(super) lowerers: &'a [Box<dyn plugins::ResolverPlugin>],
    pub(super) marker_crates: &'a [String],
    pub(super) file: &'a Path,
    pub(super) out: &'a mut Vec<Occurrence>,
}

impl<'ast, 'a> syn::visit::Visit<'ast> for NestedMacroLowering<'a> {
    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        let site = plugins::MacroSite {
            is_macro_rules: false,
            path: &mac.path,
            tokens: &mac.tokens,
            marker_crates: self.marker_crates,
        };
        if let Some(plugin) = self.lowerers.iter().find(|p| p.claims_macro(&site)) {
            let mac_span = mac
                .path
                .segments
                .first()
                .map(|s| span_to_source_span(self.file, s.ident.span()));
            let cx = plugins::LowerCtx {
                macro_span: mac_span,
            };
            match plugin.lower_macro(&site, &cx) {
                plugins::Lowered::ScanPlus(occs) | plugins::Lowered::Exact(occs) => {
                    self.out.extend(occs);
                }
                plugins::Lowered::TokenScan => {}
            }
        }
        syn::visit::visit_macro(self, mac);
    }
}
