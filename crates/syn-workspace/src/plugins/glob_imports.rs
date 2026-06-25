//! Core Phase-B pass: bind names brought into scope by a glob import
//! (`use m::*;`) when the glob target is a workspace module.
//!
//! Tier 1 deliberately emits no bindings for globs — a single file can't know
//! what `m::*` contains. That leaves two reference shapes unresolved wherever
//! a module relies on a glob import (ubiquitous in `#[cfg(test)] mod tests {
//! use super::*; }` blocks and in bench/test targets doing `use my_lib::*;`):
//!
//! - a **bare ident** the glob brought into scope (`build_index(…)` after
//!   `use my_lib::*;`) — captured at Phase A as [`Origin::GlobCandidate`]
//!   and left unresolved by the central resolver;
//! - a **multi-segment run whose root** the glob brought into scope
//!   (`helpers::run()` after `use my_lib::*;`) — resolved by the central
//!   resolver's external-crate fallback to a bogus `helpers::run`, betrayed
//!   by its resolved path equalling its raw segments.
//!
//! Once every module tree is built, the glob target *is* knowable: this pass
//! looks up the target module(s) by canonical path and binds candidates whose
//! name matches one of the target's items, submodules, or re-exported `use`
//! bindings — emitting reference edges like its structural siblings
//! [`super::macro_calls::MacroCallPass`] / `DioxusComponentPass`. Like them it
//! is **core** (always registered): glob imports are a language feature.
//!
//! ## Precision tradeoffs (all in the FP-safe direction)
//!
//! By-name binding can over-link: a local variable shadowing a glob-target
//! item name, or the same canonical hosted by several targets (every target
//! root shares the crate's code name), adds a reference edge that only ever
//! *suppresses* an unused-finding — it never creates one. Visibility is
//! deliberately ignored (a same-crate `use super::*` can genuinely see
//! private items; a cross-crate glob can't, but binding a private item's
//! name is again only suppressive). External-crate globs stay a documented
//! non-goal — their contents aren't in the model.

use std::collections::{HashMap, HashSet};

use crate::plugins::{ContributedRef, Fact, Provenance, ResolverPlugin};
use crate::resolve::{Crate, ItemKind, Module, Origin, ResolvedPath, Target};

/// Phase B pass: binds glob-imported names to the glob target's items when
/// the target is a workspace module.
pub(crate) struct GlobImportPass;

/// What a glob target module offers for by-name binding.
struct TargetSurface<'a> {
    /// Item name → canonical paths (an item name can repeat across the
    /// modules sharing one canonical, e.g. target roots).
    items: HashMap<&'a str, Vec<&'a ResolvedPath>>,
    /// Macro item name → canonical paths (for `Origin::MacroCall` binding).
    macros: HashMap<&'a str, Vec<&'a ResolvedPath>>,
    /// Submodule name → canonical paths (multi-segment roots resolve through
    /// these: `helpers::run` after `use lib::*`).
    submodules: HashMap<&'a str, Vec<&'a ResolvedPath>>,
    /// Re-exported binding local name → the binding's canonical (a glob
    /// import re-imports the target's `pub use` names).
    bindings: HashMap<&'a str, Vec<&'a ResolvedPath>>,
}

impl<'a> TargetSurface<'a> {
    fn from_modules(modules: &[&'a Module]) -> Self {
        let mut surface = Self {
            items: HashMap::new(),
            macros: HashMap::new(),
            submodules: HashMap::new(),
            bindings: HashMap::new(),
        };
        for module in modules {
            for item in &module.items {
                if !item.kind.is_definition() {
                    continue;
                }
                surface
                    .items
                    .entry(item.name.as_str())
                    .or_default()
                    .push(&item.canonical);
                if item.kind == ItemKind::Macro {
                    surface
                        .macros
                        .entry(item.name.as_str())
                        .or_default()
                        .push(&item.canonical);
                }
            }
            for sub in &module.submodules {
                surface
                    .submodules
                    .entry(sub.name.as_str())
                    .or_default()
                    .push(&sub.canonical);
            }
            for binding in &module.use_bindings {
                surface
                    .bindings
                    .entry(binding.local_name.as_str())
                    .or_default()
                    .push(&binding.canonical);
            }
        }
        surface
    }

    /// Canonical paths a bare `name` could bind to: items, submodules, and
    /// re-exported binding names, in that order.
    fn lookup(&self, name: &str) -> impl Iterator<Item = &'a ResolvedPath> + '_ {
        self.items
            .get(name)
            .into_iter()
            .chain(self.submodules.get(name))
            .chain(self.bindings.get(name))
            .flatten()
            .copied()
    }
}

impl ResolverPlugin for GlobImportPass {
    fn global_facts(&self, crates: &[Crate]) -> Vec<Fact> {
        // Canonical module path → every member module carrying it. Target
        // roots all carry the crate's code name, so one canonical can map to
        // several modules (lib root + bench roots, …) — binding against the
        // union is the documented over-link tradeoff.
        let mut modules_by_canonical: HashMap<&ResolvedPath, Vec<&Module>> = HashMap::new();
        for krate in crates {
            if !krate.is_workspace_member {
                continue;
            }
            for module in krate.all_modules() {
                modules_by_canonical
                    .entry(&module.canonical)
                    .or_default()
                    .push(module);
            }
        }

        let mut seen: HashSet<(String, ResolvedPath, bool)> = HashSet::new();
        let mut out = Vec::new();
        for krate in crates {
            if !krate.is_workspace_member {
                continue;
            }
            let from = krate.code_name();
            let primary = krate.lib_or_main().map(|t| t as *const Target);
            for target in &krate.targets {
                let via_sibling_target = primary != Some(target as *const Target);
                for module in target.root.walk() {
                    let glob_targets: Vec<&ResolvedPath> = module
                        .occurrences
                        .iter()
                        .filter(|o| o.origin == Origin::GlobUse)
                        .filter_map(|o| o.path.as_ref())
                        .collect();
                    if glob_targets.is_empty() {
                        continue;
                    }
                    let target_modules: Vec<&Module> = glob_targets
                        .iter()
                        .filter_map(|p| modules_by_canonical.get(*p))
                        .flatten()
                        .copied()
                        .collect();
                    if target_modules.is_empty() {
                        continue;
                    }
                    let surface = TargetSurface::from_modules(&target_modules);
                    for occ in &module.occurrences {
                        let bound: Vec<ResolvedPath> = match occ.origin {
                            // A bare ident the glob may have brought into scope.
                            Origin::GlobCandidate => {
                                let Some(name) = occ.segments.first() else {
                                    continue;
                                };
                                surface.lookup(name).cloned().collect()
                            }
                            // A bare macro invocation: the same-crate
                            // MacroCallPass covers local macro_rules!; this
                            // binds the glob-imported (`use other::*`) form.
                            Origin::MacroCall => {
                                let Some(name) = occ.segments.first() else {
                                    continue;
                                };
                                surface
                                    .macros
                                    .get(name.as_str())
                                    .into_iter()
                                    .flatten()
                                    .map(|p| (*p).clone())
                                    .collect()
                            }
                            // A multi-segment run whose root resolved via the
                            // external-crate fallback (resolved == raw): if the
                            // glob brought the root into scope, the real target
                            // is `<bound root>::rest…`.
                            Origin::Code => {
                                let untouched = occ
                                    .path
                                    .as_ref()
                                    .is_some_and(|p| p.segments() == occ.segments);
                                if !untouched || occ.segments.len() < 2 {
                                    continue;
                                }
                                let root = &occ.segments[0];
                                surface
                                    .lookup(root)
                                    .map(|base| {
                                        let mut segs = base.segments().to_vec();
                                        segs.extend(occ.segments[1..].iter().cloned());
                                        ResolvedPath::new(segs)
                                    })
                                    .collect()
                            }
                            _ => continue,
                        };
                        for to in bound {
                            if seen.insert((from.clone(), to.clone(), via_sibling_target)) {
                                out.push(ContributedRef {
                                    from: from.clone(),
                                    to,
                                    via_sibling_target,
                                });
                            }
                        }
                    }
                }
            }
        }
        out.into_iter()
            .map(|edge| Fact::Reference {
                edge,
                by: Provenance {
                    plugin: "glob_imports",
                    rule: "glob-binding",
                    trigger: None,
                },
            })
            .collect()
    }
}
