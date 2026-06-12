//! `use`-binding helpers for the module-tree walk: function-body-local `use`
//! collection, the sibling-prefix rewrite shared by module-level and nested
//! uses, the Tier-1 scope projection, and `pub use M::*;` glob re-export target
//! canonicalization.

use std::collections::HashSet;
use std::path::Path;

use super::use_tree::{self, UseBinding};
use super::{ResolvedPath, Visibility};

/// Canonicalized targets of a `pub use M::*;` glob re-export, or empty for a
/// private (`use M::*;`) glob — private globs import, they don't re-export.
pub(super) fn pub_glob_reexport_targets(
    item_use: &syn::ItemUse,
    targets: &[ResolvedPath],
    parent_canonical: &ResolvedPath,
    sibling_names: &HashSet<String>,
) -> Vec<ResolvedPath> {
    if !matches!(Visibility::from_syn(&item_use.vis), Visibility::Public) {
        return Vec::new();
    }
    targets
        .iter()
        .map(|t| canonicalize_glob_target(t, parent_canonical, sibling_names))
        .collect()
}

/// Canonicalize a glob re-export target prefix to a full module path.
/// `glob_targets_from_use` already peels `crate::`/`self::`/`super::` (those
/// land with the crate name as the leading segment), but a bare leading segment
/// that names a sibling module — `pub use inner::*` — still needs this module's
/// canonical prepended. A leading segment equal to the crate name is already
/// anchored; anything else (an external crate) is left as-is.
fn canonicalize_glob_target(
    target: &ResolvedPath,
    parent_canonical: &ResolvedPath,
    siblings: &HashSet<String>,
) -> ResolvedPath {
    let crate_name = parent_canonical.segments().first();
    match target.segments().first() {
        Some(first) if Some(first) != crate_name && siblings.contains(first) => {
            let mut segs = parent_canonical.segments().to_vec();
            segs.extend(target.segments().iter().cloned());
            ResolvedPath::new(segs)
        }
        _ => target.clone(),
    }
}

/// Collects `use` statements nested inside item bodies (fn / impl-method
/// blocks, nested blocks) so their bindings can resolve later paths in the same
/// module. Deliberately does **not** descend into nested `mod` items — those own
/// their own scope and are resolved by the submodule recursion in
/// [`collect_module_contents`].
#[derive(Default)]
struct NestedUseCollector<'ast> {
    uses: Vec<&'ast syn::ItemUse>,
}

impl<'ast> syn::visit::Visit<'ast> for NestedUseCollector<'ast> {
    fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
        self.uses.push(node);
    }

    fn visit_item_mod(&mut self, _node: &'ast syn::ItemMod) {
        // Stop: a nested module's `use`s belong to *its* scope, and the file/
        // inline-module recursion already processes them there.
    }
}

/// Bindings from every `use` statement nested inside an item body (fn /
/// impl-method blocks). Module-scoped — a slight over-approximation that only
/// *adds* references the code already makes (it can't invent a cross-crate ref
/// out of nothing), so the cross-crate SCIP precision gate is unaffected,
/// mirroring the sibling-name broadening. Module-level `use` items are handled
/// in the main pass; nested `mod`s carry their own scope and are skipped here.
pub(super) fn function_local_use_bindings(
    syn_items: &[syn::Item],
    scope: &use_tree::Scope,
    parent_file: &Path,
    parent_canonical: &ResolvedPath,
    sibling_names: &HashSet<String>,
) -> Vec<UseBinding> {
    let mut out = Vec::new();
    for syn_item in syn_items {
        if matches!(syn_item, syn::Item::Use(_) | syn::Item::Mod(_)) {
            continue;
        }
        let mut collector = NestedUseCollector::default();
        syn::visit::Visit::visit_item(&mut collector, syn_item);
        for item_use in collector.uses {
            let mut bindings = use_tree::bindings_from_use(item_use, scope, parent_file);
            for binding in &mut bindings {
                rewrite_sibling_local(binding, parent_canonical, sibling_names);
            }
            out.extend(bindings);
        }
    }
    out
}

/// If `binding`'s canonical path starts with a name that's declared in the
/// surrounding module (a sibling), prepend the surrounding module's path so
/// the canonical resolves crate-local instead of being treated as an
/// external crate.
pub(super) fn rewrite_sibling_local(
    binding: &mut UseBinding,
    parent_canonical: &ResolvedPath,
    siblings: &HashSet<String>,
) {
    let Some(first) = binding.canonical.segments().first() else {
        return;
    };
    if !siblings.contains(first) {
        return;
    }
    let mut new_segs = parent_canonical.segments().to_vec();
    new_segs.extend(binding.canonical.segments().iter().cloned());
    binding.canonical = ResolvedPath::new(new_segs);
}

pub(super) fn scope_from(canonical: &ResolvedPath) -> use_tree::Scope {
    let segs = canonical.segments();
    let crate_name = segs.first().cloned().unwrap_or_default();
    let module_path = segs.get(1..).map(<[String]>::to_vec).unwrap_or_default();
    use_tree::Scope {
        crate_name,
        module_path,
    }
}
