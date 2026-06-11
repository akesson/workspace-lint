//! Match reference-evidence findings against the SCIP index and mutate the
//! disproved ones in place. One-directional: a finding is either **confirmed**
//! (rust-analyzer agrees the item/dep is unreferenced — the structural fix
//! stands) or **disproved** (rust-analyzer sees a reference the resolver
//! missed — downgrade the fix and queue an `expect` directive). SCIP never
//! turns a non-finding into a finding.

use std::path::{Path, PathBuf};

use syn_workspace::Workspace;

use super::directive;
use super::index::{Occurrence, ScipIndex};
use super::normalize::is_prefix;
use crate::diagnostic::{Applicability, Diagnostic, Evidence, Level, PubVerdict, Suggestion};

/// What verification did, for the summary line and to drive `fix::run`.
#[derive(Default)]
pub(crate) struct Outcome {
    pub confirmed: usize,
    pub disproved: usize,
    /// Directive insertions to apply alongside the surviving structural fixes.
    pub inserts: Vec<Suggestion>,
}

/// Verify every `MachineApplicable`, evidence-bearing suggestion against
/// `index`, mutating `diagnostics` in place: a disproved suggestion is
/// downgraded to `MaybeIncorrect` (so `fix::run` skips it) and its finding is
/// annotated; a directive insertion is queued for it. A diagnostic *all* of
/// whose fixes were disproved is relabelled a resolver false positive and
/// pinned to `warn` so it can't fail the build.
pub(crate) fn verify(
    diagnostics: &mut [Diagnostic],
    index: &ScipIndex,
    workspace: &Workspace,
) -> Outcome {
    let map = CrateMap::build(workspace);
    let mut outcome = Outcome::default();

    for d in diagnostics.iter_mut() {
        let mut total = 0usize;
        let mut disproved_here = 0usize;
        let mut notes: Vec<String> = Vec::new();

        for s in d.suggestions.iter_mut() {
            if s.applicability != Applicability::MachineApplicable {
                continue;
            }
            let Some(evidence) = s.evidence.clone() else {
                continue;
            };
            total += 1;
            let Some(occ) = first_disproving(&evidence, index, &map) else {
                continue; // confirmed
            };
            disproved_here += 1;
            s.applicability = Applicability::MaybeIncorrect;
            let prov = provenance(&evidence, occ);
            notes.push(format!(
                "rust-analyzer disproved {}: {prov} — wrote an `expect` directive",
                describe(&evidence)
            ));
            if let Some(insert) = directive_for(s, &evidence, &prov) {
                outcome.inserts.push(insert);
            }
        }

        outcome.confirmed += total - disproved_here;
        outcome.disproved += disproved_here;
        for n in notes {
            d.notes.push(n);
        }
        // Only relabel/de-escalate when the *entire* finding is a false positive;
        // a partially-disproved aggregate (some deps genuinely unused) keeps its
        // level and just carries per-dep notes.
        if total > 0 && disproved_here == total {
            d.message = format!(
                "resolver false positive (disproved by rust-analyzer): {}",
                d.message
            );
            d.notes.push(
                "consider a tests/cases known_false_positives fixture for this resolver gap".into(),
            );
            d.level = Level::Warn;
            d.level_is_explicit = true;
        }
    }

    outcome
}

/// Build the directive insertion for a disproved suggestion, anchored at the
/// suggestion's own source line (the dep line, or the item line).
fn directive_for(s: &Suggestion, evidence: &Evidence, prov: &str) -> Option<Suggestion> {
    let lint = match evidence {
        Evidence::DepUnused { .. } => "unused-deps",
        Evidence::PubUnused { .. } => "unused-pub",
    };
    directive::build_expect_insert(&s.span.file, s.span.line_start, lint, prov)
}

/// The first SCIP occurrence that disproves this finding, if any.
fn first_disproving<'a>(
    evidence: &Evidence,
    index: &'a ScipIndex,
    map: &CrateMap,
) -> Option<&'a Occurrence> {
    match evidence {
        Evidence::DepUnused {
            krate_code,
            package_name,
        } => {
            let want = package_name.replace('-', "_");
            let want_stripped = separator_stripped(&want);
            index.occurrences.iter().find(|o| {
                map.owner(&o.file).as_deref() == Some(krate_code.as_str())
                    && (o.symbol.package == want
                        || separator_stripped(&o.symbol.package) == want_stripped)
            })
        }
        Evidence::PubUnused {
            krate_code,
            canonical,
            verdict,
        } => index.occurrences.iter().find(|o| {
            if o.is_definition || !is_prefix(canonical, &o.symbol.segments) {
                return false;
            }
            match verdict {
                // Any reference anywhere disproves "unused".
                PubVerdict::Unused => true,
                // Tightening to `pub(crate)` only breaks a use from *outside* the
                // crate's primary lib: a different crate, or a sibling target
                // (test/bench/example) that links the lib externally.
                PubVerdict::IntraCrate => {
                    map.owner(&o.file).as_deref() != Some(krate_code.as_str())
                        || !map.in_primary_src(&o.file)
                }
            }
        }),
    }
}

/// Human-readable provenance trailer for the written directive and the note.
fn provenance(evidence: &Evidence, occ: &Occurrence) -> String {
    let what = match evidence {
        Evidence::DepUnused { package_name, .. } => format!("`{package_name}`"),
        Evidence::PubUnused { canonical, .. } => format!("`{}`", canonical.join("::")),
    };
    // SCIP lines are 0-based; show 1-based to match editors.
    let loc = match occ.line {
        Some(l) => format!("{}:{}", occ.file, l + 1),
        None => occ.file.clone(),
    };
    format!("rust-analyzer sees {what} referenced ({loc})")
}

fn describe(evidence: &Evidence) -> String {
    match evidence {
        Evidence::DepUnused { package_name, .. } => format!("dependency `{package_name}`"),
        Evidence::PubUnused { canonical, .. } => format!("`{}`", canonical.join("::")),
    }
}

/// A crate name with `-`/`_` removed — the FP-safe lib-name fallback
/// (`md_5` ↔ `md5`). Mirrors the one in `unused_deps`.
fn separator_stripped(name: &str) -> String {
    name.chars().filter(|c| *c != '-' && *c != '_').collect()
}

/// Maps a SCIP document path (workspace-root-relative, `/`-separated) to its
/// owning workspace member, and tells whether it sits in that member's primary
/// `src/` tree (vs a sibling target like `tests/` or `benches/`).
struct CrateMap {
    /// (manifest dir relative to workspace root, crate code name), most-specific
    /// (deepest) first so nested members win.
    entries: Vec<(PathBuf, String)>,
}

impl CrateMap {
    fn build(ws: &Workspace) -> Self {
        let mut entries: Vec<(PathBuf, String)> = ws
            .members()
            .map(|c| (ws.crate_relative_path(&c.manifest_dir), c.code_name()))
            .collect();
        entries.sort_by_key(|(p, _)| std::cmp::Reverse(p.components().count()));
        Self { entries }
    }

    fn entry_for(&self, file: &str) -> Option<&(PathBuf, String)> {
        let path = Path::new(file);
        self.entries
            .iter()
            .find(|(dir, _)| !dir.as_os_str().is_empty() && path.starts_with(dir))
    }

    fn owner(&self, file: &str) -> Option<String> {
        self.entry_for(file).map(|(_, name)| name.clone())
    }

    fn in_primary_src(&self, file: &str) -> bool {
        self.entry_for(file)
            .map(|(dir, _)| Path::new(file).starts_with(dir.join("src")))
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(pkg: &str, segs: &[&str]) -> super::super::normalize::NormalizedSymbol {
        super::super::normalize::NormalizedSymbol {
            package: pkg.to_string(),
            segments: segs.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn occ(file: &str, pkg: &str, segs: &[&str], is_def: bool) -> Occurrence {
        Occurrence {
            file: file.to_string(),
            line: Some(0),
            symbol: norm(pkg, segs),
            is_definition: is_def,
        }
    }

    fn index(occs: Vec<Occurrence>) -> ScipIndex {
        ScipIndex { occurrences: occs }
    }

    fn map() -> CrateMap {
        // Two members: demo at crates/demo, util at crates/util.
        CrateMap {
            entries: vec![
                (PathBuf::from("crates/demo"), "demo".to_string()),
                (PathBuf::from("crates/util"), "util".to_string()),
            ],
        }
    }

    #[test]
    fn dep_disproved_when_package_referenced_in_owning_crate() {
        let ev = Evidence::DepUnused {
            krate_code: "demo".into(),
            package_name: "strum".into(),
        };
        let idx = index(vec![occ(
            "crates/demo/src/lib.rs",
            "strum",
            &["strum", "EnumString"],
            false,
        )]);
        assert!(first_disproving(&ev, &idx, &map()).is_some());
    }

    #[test]
    fn dep_confirmed_when_reference_in_other_crate() {
        // strum referenced from `util`, but the finding is for `demo`'s dep.
        let ev = Evidence::DepUnused {
            krate_code: "demo".into(),
            package_name: "strum".into(),
        };
        let idx = index(vec![occ(
            "crates/util/src/lib.rs",
            "strum",
            &["strum", "EnumString"],
            false,
        )]);
        assert!(first_disproving(&ev, &idx, &map()).is_none());
    }

    #[test]
    fn dep_md5_libname_matches_via_separator_fallback() {
        // Defensive: even if SCIP carried `md5` (no hyphen) for an `md-5` dep.
        let ev = Evidence::DepUnused {
            krate_code: "demo".into(),
            package_name: "md-5".into(),
        };
        let idx = index(vec![occ(
            "crates/demo/src/lib.rs",
            "md5",
            &["md5", "Md5"],
            false,
        )]);
        assert!(first_disproving(&ev, &idx, &map()).is_some());
    }

    #[test]
    fn pub_unused_disproved_by_any_reference() {
        let ev = Evidence::PubUnused {
            krate_code: "demo".into(),
            canonical: vec!["demo".into(), "Thing".into()],
            verdict: PubVerdict::Unused,
        };
        let idx = index(vec![occ(
            "crates/util/src/lib.rs",
            "demo",
            &["demo", "Thing"],
            false,
        )]);
        assert!(first_disproving(&ev, &idx, &map()).is_some());
    }

    #[test]
    fn pub_unused_own_definition_does_not_disprove() {
        let ev = Evidence::PubUnused {
            krate_code: "demo".into(),
            canonical: vec!["demo".into(), "Thing".into()],
            verdict: PubVerdict::Unused,
        };
        // Only the item's own definition exists — not a reference.
        let idx = index(vec![occ(
            "crates/demo/src/lib.rs",
            "demo",
            &["demo", "Thing"],
            true,
        )]);
        assert!(first_disproving(&ev, &idx, &map()).is_none());
    }

    #[test]
    fn pub_unused_disproved_by_method_reference() {
        // A `Thing::new()` reference (method) proves `Thing` is used.
        let ev = Evidence::PubUnused {
            krate_code: "demo".into(),
            canonical: vec!["demo".into(), "Thing".into()],
            verdict: PubVerdict::Unused,
        };
        let idx = index(vec![occ(
            "crates/util/src/lib.rs",
            "demo",
            &["demo", "Thing", "new"],
            false,
        )]);
        assert!(first_disproving(&ev, &idx, &map()).is_some());
    }

    #[test]
    fn intra_crate_not_disproved_by_same_crate_src_reference() {
        // A same-crate src reference is consistent with tightening to pub(crate).
        let ev = Evidence::PubUnused {
            krate_code: "demo".into(),
            canonical: vec!["demo".into(), "Thing".into()],
            verdict: PubVerdict::IntraCrate,
        };
        let idx = index(vec![occ(
            "crates/demo/src/other.rs",
            "demo",
            &["demo", "Thing"],
            false,
        )]);
        assert!(first_disproving(&ev, &idx, &map()).is_none());
    }

    #[test]
    fn intra_crate_disproved_by_other_crate_reference() {
        let ev = Evidence::PubUnused {
            krate_code: "demo".into(),
            canonical: vec!["demo".into(), "Thing".into()],
            verdict: PubVerdict::IntraCrate,
        };
        let idx = index(vec![occ(
            "crates/util/src/lib.rs",
            "demo",
            &["demo", "Thing"],
            false,
        )]);
        assert!(first_disproving(&ev, &idx, &map()).is_some());
    }

    #[test]
    fn intra_crate_disproved_by_sibling_target_reference() {
        // A reference from demo's own integration test (links the lib
        // externally) — tightening would break it.
        let ev = Evidence::PubUnused {
            krate_code: "demo".into(),
            canonical: vec!["demo".into(), "Thing".into()],
            verdict: PubVerdict::IntraCrate,
        };
        let idx = index(vec![occ(
            "crates/demo/tests/it.rs",
            "demo",
            &["demo", "Thing"],
            false,
        )]);
        assert!(first_disproving(&ev, &idx, &map()).is_some());
    }
}
