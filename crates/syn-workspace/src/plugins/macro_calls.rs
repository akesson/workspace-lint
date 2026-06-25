//! Core Phase-B pass: bind a bare macro invocation `foo!(…)` to the same-crate
//! `macro_rules! foo` definition.
//!
//! A bare single-ident macro invocation in regular code is captured as an
//! [`Origin::MacroCall`] occurrence (see [`crate::resolve`]'s `module_tree`),
//! left unresolved by the central path resolver: an exported `macro_rules!`
//! lives in the *macro* namespace and is crate-global, not reachable by a module
//! path. This pass — a structural sibling of [`super::dioxus_rsx`]'s
//! `DioxusComponentPass` — reads the resolved model and emits a reference edge
//! from each `MacroCall` occurrence to the matching same-crate macro definition,
//! so an exported macro used only via intra-crate invocation isn't
//! false-positived by `unused-pub`.
//!
//! Unlike the Dioxus pass this is **core** (always registered): `macro_rules!`
//! is a language feature, not a framework one.
//!
//! ## Scope: same-crate only
//!
//! A bare name is bound to `macro_rules!` definitions in its own crate. A
//! cross-crate macro reached through `use other::mac;` already counts as a
//! reference via its `use` binding, and an `other::mac!()` invocation is an
//! ordinary multi-segment [`Origin::Code`] run — neither needs this pass.
//! Binding by bare name can over-link a name shared by two same-crate macros
//! (both definitions get the edge), but that only ever *suppresses* an
//! unused-finding — the FP-safe direction, mirroring the component pass.

use std::collections::HashMap;

use crate::plugins::{ContributedRef, Fact, Provenance, ResolverPlugin};
use crate::resolve::{Crate, ItemKind, Origin, ResolvedPath};

/// Phase B pass: binds a bare `foo!(…)` invocation to the same-crate
/// `macro_rules! foo` of that name.
pub(crate) struct MacroCallPass;

impl ResolverPlugin for MacroCallPass {
    fn global_facts(&self, crates: &[Crate]) -> Vec<Fact> {
        let mut out = Vec::new();
        for krate in crates {
            if !krate.is_workspace_member {
                continue;
            }
            // Candidate macro definitions: every public macro, keyed by bare name
            // (a name may be defined in more than one module).
            let mut defs: HashMap<&str, Vec<&ResolvedPath>> = HashMap::new();
            for item in krate.pub_items() {
                if item.kind == ItemKind::Macro {
                    defs.entry(item.name.as_str())
                        .or_default()
                        .push(&item.canonical);
                }
            }
            if defs.is_empty() {
                continue;
            }
            let from = krate.code_name();
            // Bare macro invocations captured as Origin::MacroCall occurrences.
            for module in krate.all_modules() {
                for occ in &module.occurrences {
                    if occ.origin != Origin::MacroCall {
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
                                // Macros aren't `pub`-narrowable items, so
                                // sibling-target provenance buys nothing here.
                                via_sibling_target: false,
                            });
                        }
                    }
                }
            }
        }
        out.into_iter().map(reference_fact).collect()
    }
}

/// Wrap a discovered edge as a [`Fact::Reference`] tagged with this pass's provenance.
fn reference_fact(edge: ContributedRef) -> Fact {
    Fact::Reference {
        edge,
        by: Provenance {
            plugin: "macro_calls",
            rule: "macro-call",
            trigger: None,
        },
    }
}
