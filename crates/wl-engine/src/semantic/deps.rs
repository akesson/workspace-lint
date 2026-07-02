//! The unused-deps verdict: declared dependencies vs the reference graph,
//! unioned across configs. Lifted from the spike assembler's
//! `report_unused_deps`, producing data instead of a printed report.
//!
//! Judgement scope (honest limits, surfaced in the result so the lint can
//! report what was and wasn't checked):
//!   * **normal** deps — always judged (lib/bin compile in every config);
//!   * **dev** deps — judged only when a test/example/bench target was
//!     compiled (a `--tests` config present); else never flagged;
//!   * **build** deps — never judged (`build.rs` isn't lint-passed);
//!   * **optional** deps — never judged (feature-gated).
//!
//! Facade crates are handled: references resolve to the *defining* crate
//! (`use clap::Parser` edges point at `clap_builder`), so a declared dep
//! counts as used when the referenced-crate set intersects its resolved
//! dependency **closure** ([`WorkspaceMeta::dep_closure`]).
//!
//! Two residual blind spots stay stated, not hidden: **macro-only deps** (a
//! bare `serde_derive` whose expansion references `serde`) and **side-effect
//! deps** with no API surface (`cargo-husky`) — the lint's `ignore` config is
//! the production answer for those.

use std::collections::{BTreeMap, BTreeSet};

use super::assembly::Assembly;
use super::meta::{DepKind, WorkspaceMeta};

/// One member's unused-deps verdict.
#[derive(Debug)]
pub struct CrateDeps {
    pub krate: String,
    /// Judged-and-unexercised declared deps (code-form names) — the findings.
    pub unused: Vec<UnusedDep>,
    /// Declared deps that could NOT be judged under the provided configs,
    /// with the reason — never findings.
    pub not_judged: Vec<(String, NotJudged)>,
}

#[derive(Debug)]
pub struct UnusedDep {
    /// Code-form package name of the dependency.
    pub name: String,
    pub kind: DepKind,
}

/// Why a declared dep was exempt from judgement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotJudged {
    /// Dev dep and no test/example/bench target was compiled — pass a
    /// `--tests` config to judge these.
    DevWithoutTestConfig,
    /// `build.rs` dependency: build scripts aren't lint-passed (no fragment).
    BuildDep,
    /// `optional = true`: feature-gated, not compiled unless enabled.
    Optional,
}

/// The workspace-wide unused-deps verdict.
#[derive(Debug)]
pub struct DepsVerdict {
    pub crates: Vec<CrateDeps>,
    /// Whether any test/example/bench target was compiled — i.e. whether dev
    /// deps were judgeable at all.
    pub dev_deps_judged: bool,
}

impl DepsVerdict {
    pub(super) fn compute(configs: &[(String, Assembly)], meta: &WorkspaceMeta) -> DepsVerdict {
        // exercised[pkg] = union, over every config and every target the
        // package owns, of the crate-names that target references. A dep is
        // declared once per package but may be used by any of its targets.
        let mut exercised: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut compiled_test_target = false;
        for (_, asm) in configs {
            for frag in asm.fragments() {
                if meta.test_targets.contains(&frag.crate_name) {
                    compiled_test_target = true;
                }
                let Some(owner) = meta.target_owner.get(&frag.crate_name) else {
                    continue; // no matching manifest target — don't guess
                };
                let set = exercised.entry(owner.clone()).or_default();
                for e in &frag.references {
                    if let Some(to) = e.to.first()
                        && to != owner
                    {
                        set.insert(to.clone());
                    }
                }
            }
        }

        let mut crates = Vec::new();
        for member in &meta.members {
            let Some(decls) = meta.declared.get(member).filter(|d| !d.is_empty()) else {
                continue; // no declared deps — nothing to say
            };
            let used = exercised.get(member).cloned().unwrap_or_default();

            let mut unused = Vec::new();
            let mut not_judged = Vec::new();
            for d in decls {
                let exempt = if d.optional {
                    Some(NotJudged::Optional)
                } else {
                    match d.kind {
                        DepKind::Build => Some(NotJudged::BuildDep),
                        DepKind::Dev if !compiled_test_target => {
                            Some(NotJudged::DevWithoutTestConfig)
                        }
                        _ => None,
                    }
                };
                if let Some(reason) = exempt {
                    not_judged.push((d.name.clone(), reason));
                    continue;
                }
                // Exercised iff the referenced-crate set meets the dep's
                // resolved closure — clears facade crates soundly.
                let hit = meta.dep_closure(&d.name).iter().any(|c| used.contains(c));
                if !hit {
                    unused.push(UnusedDep {
                        name: d.name.clone(),
                        kind: d.kind,
                    });
                }
            }
            unused.sort_by(|a, b| a.name.cmp(&b.name));
            crates.push(CrateDeps {
                krate: member.clone(),
                unused,
                not_judged,
            });
        }
        DepsVerdict {
            crates,
            dev_deps_judged: compiled_test_target,
        }
    }
}
