//! The cfg-matrix union (SPIKE §7): a pub item is a lead iff unreached in
//! EVERY config. Candidates come from each crate's **home** config (the
//! first config whose lib/bin units cover it — see
//! [`covering_configs`](super::covering_configs)), so a test
//! config's integration-test crates contribute *usage* but never
//! *candidates*, while a crate only a `--target` config compiles is still
//! judged. Lifted from the spike assembler's `Matrix`, producing data
//! instead of a printed report.

use std::collections::{BTreeMap, BTreeSet};

use super::assembly::{Assembly, CandReach, Category};
use super::covering_configs;
use super::meta::WorkspaceMeta;

/// One surviving lead of the union: a pub candidate reached in no config.
#[derive(Debug)]
pub struct Lead {
    /// The cross-config identity (`crate::module::…::name`).
    pub id: String,
    pub krate: String,
    pub kind: String,
    pub category: Category,
    /// `true` ⇒ a hard verdict: the crate's pub API has no external boundary
    /// (bin / `publish = false`), so nothing anywhere can reach this item.
    /// `false` ⇒ published API surface: external consumers are possible —
    /// review for over-exposure, not death.
    pub dead: bool,
}

/// A primary-config lead retired by usage under another config — the
/// over-report the union exists to clear.
#[derive(Debug)]
pub struct Retired {
    pub id: String,
    /// The config whose usage retired it.
    pub saved_by: String,
}

/// The union verdict across the whole config matrix.
#[derive(Debug)]
pub struct UnionVerdict {
    pub leads: Vec<Lead>,
    pub retired: Vec<Retired>,
    /// Leads the primary config alone would have reported (the delta the
    /// matrix bought is `primary_only_leads - leads.len()`).
    pub primary_only_leads: usize,
}

impl UnionVerdict {
    pub(super) fn compute(
        configs: &[(String, Assembly)],
        meta: Option<&WorkspaceMeta>,
    ) -> UnionVerdict {
        let covering = covering_configs(configs);
        let is_member = |krate: &str| covering.contains_key(krate);

        // Level-1→2: reduce each config to config-stable candidate identities.
        let per: Vec<(&str, BTreeMap<String, CandReach>)> = configs
            .iter()
            .map(|(n, a)| (n.as_str(), a.candidate_identities()))
            .collect();

        // Union of reached identities across ALL configs (any crate — a test
        // crate referencing a member is real usage of that member). Foreign
        // reach counts too: a config that never extracted the defining crate
        // (harness flag off) credits the identity directly.
        let mut used: BTreeSet<&str> = BTreeSet::new();
        for (_, cands) in &per {
            for (id, c) in cands {
                if c.reached {
                    used.insert(id.as_str());
                }
            }
        }
        for (_, asm) in configs {
            for (id, fr) in &asm.degrees.foreign_reach {
                if fr.reached() {
                    used.insert(id.as_str());
                }
            }
        }

        // Union candidate set, restricted to home-covered crates (the real
        // pub API — test/bench target crates have no home).
        let mut all: BTreeMap<&str, &CandReach> = BTreeMap::new();
        for (_, cands) in &per {
            for (id, c) in cands {
                if is_member(&c.krate) {
                    all.entry(id.as_str()).or_insert(c);
                }
            }
        }

        // Surviving leads: candidate somewhere, reached nowhere — split by the
        // external-boundary classification (published lib ⇒ API surface;
        // bin / publish=false ⇒ hard DEAD verdict), judged by the crate's
        // home config.
        let leads: Vec<Lead> = all
            .iter()
            .filter(|(id, _)| !used.contains(*id))
            .map(|(id, c)| Lead {
                id: (*id).to_string(),
                krate: c.krate.clone(),
                kind: c.kind.clone(),
                category: c.category,
                dead: !configs[covering[&c.krate]]
                    .1
                    .external_boundary(&c.krate, meta),
            })
            .collect();

        // Retired: a primary-config lead used under another config (via its
        // candidate reach or its foreign reach — index-aligned with `per`).
        let primary_cands = &per[0].1;
        let mut retired = Vec::new();
        for (id, c) in primary_cands {
            if is_member(&c.krate) && !c.reached && used.contains(id.as_str()) {
                let saved_by = per
                    .iter()
                    .enumerate()
                    .skip(1)
                    .find(|(i, (_, cs))| {
                        cs.get(id).map(|x| x.reached).unwrap_or(false)
                            || configs[*i]
                                .1
                                .degrees
                                .foreign_reach
                                .get(id)
                                .is_some_and(|fr| fr.reached())
                    })
                    .map(|(_, (n, _))| (*n).to_string())
                    .unwrap_or_else(|| "?".to_string());
                retired.push(Retired {
                    id: id.clone(),
                    saved_by,
                });
            }
        }

        let primary_only_leads = primary_cands
            .iter()
            .filter(|(_, c)| is_member(&c.krate) && !c.reached)
            .count();

        UnionVerdict {
            leads,
            retired,
            primary_only_leads,
        }
    }
}
