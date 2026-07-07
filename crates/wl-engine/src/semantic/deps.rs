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

/// The per-package exercised-crate sets — the reusable primitive under both
/// [`DepsVerdict`] and the ported `unused-deps` lint (which layers its own
/// manifest-driven judgement — ignore config, doctest refs, feature plumbing —
/// on top of this semantic signal).
pub struct DepUsage {
    /// pkg → union, over every config and every target the package owns, of
    /// the crate-names that target references. A dep is declared once per
    /// package but may be used by any of its targets.
    exercised: BTreeMap<String, BTreeSet<String>>,
    /// Whether any test/example/bench target was compiled — dev deps are
    /// judgeable only then.
    dev_deps_judged: bool,
}

impl DepUsage {
    pub(super) fn compute(configs: &[(String, Assembly)], meta: &WorkspaceMeta) -> DepUsage {
        let mut exercised: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut dev_deps_judged = false;
        for (_, asm) in configs {
            for frag in asm.archived_fragments() {
                if meta.test_targets.contains(frag.crate_name.as_str()) {
                    dev_deps_judged = true;
                }
                let Some(owner) = meta.target_owner.get(frag.crate_name.as_str()) else {
                    continue; // no matching manifest target — don't guess
                };
                let set = exercised.entry(owner.clone()).or_default();
                for e in frag.references.iter() {
                    if let Some(to) = e.to.first()
                        && to.as_str() != owner.as_str()
                    {
                        set.insert(to.as_str().to_owned());
                    }
                    // `to[0]` is the *defining* crate; a re-export shim
                    // (`use web_time::Instant` resolving into `std`) is only
                    // named by the written path root the extractor records
                    // in `via` — credit it too, or the shim dep reads unused.
                    if let Some(via) = e.via.as_deref()
                        && via != owner.as_str()
                    {
                        set.insert(via.to_owned());
                    }
                }
            }
        }
        DepUsage {
            exercised,
            dev_deps_judged,
        }
    }

    /// Is the declared dep `dep_pkg` (package code-name) exercised by any of
    /// `owner`'s targets under any config? Facade- and lib-rename-aware: true
    /// iff the owner's referenced-crate set meets the dep's resolved closure
    /// (which carries both package and lib-target names).
    pub fn dep_used(&self, meta: &WorkspaceMeta, owner: &str, dep_pkg: &str) -> bool {
        let Some(used) = self.exercised.get(owner) else {
            return false;
        };
        meta.dep_closure(dep_pkg).iter().any(|c| used.contains(c))
    }

    pub fn dev_deps_judged(&self) -> bool {
        self.dev_deps_judged
    }
}

impl DepsVerdict {
    pub(super) fn compute(configs: &[(String, Assembly)], meta: &WorkspaceMeta) -> DepsVerdict {
        let usage = DepUsage::compute(configs, meta);
        let compiled_test_target = usage.dev_deps_judged;

        let mut crates = Vec::new();
        for member in &meta.members {
            let Some(decls) = meta.declared.get(member).filter(|d| !d.is_empty()) else {
                continue; // no declared deps — nothing to say
            };

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
                if !usage.dep_used(meta, member, &d.name) {
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
