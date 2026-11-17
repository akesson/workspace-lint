//! [`EdgeFold`]: the edge-derived indexes one pass of
//! [`Assembly::fold_edges`](super::Assembly::fold_edges) produces — the degree maps'
//! single write points, so the visibility, provenance and test-user splits
//! can never drift apart.

use std::collections::{BTreeMap, BTreeSet};

use super::join::ForeignReach;
use super::pub_usage::TestUser;
use super::removal::DegreeMaps;

/// The edge-derived indexes produced by one pass of
/// [`Assembly::fold_edges`](super::Assembly::fold_edges) — the reverse index the reachability verdicts read.
#[derive(Default)]
pub(super) struct EdgeFold {
    pub(super) in_degree: BTreeMap<String, usize>,
    pub(super) intra_degree: BTreeMap<String, usize>,
    /// The subset of `in_degree` contributed by **test** units (a `+test` cfg
    /// variant, an integration test, or a bench — see
    /// [`Assembly::is_test_unit`](super::Assembly::is_test_unit)). `in_degree - test_in_degree` is the
    /// production reach unused-pub's test-only classification judges.
    pub(super) test_in_degree: BTreeMap<String, usize>,
    /// The subset of `intra_degree` contributed by test units.
    pub(super) test_intra_degree: BTreeMap<String, usize>,
    pub(super) referenced: BTreeSet<String>,
    pub(super) import_edges: usize,
    pub(super) signature_exposed: BTreeSet<String>,
    pub(super) import_targets: BTreeSet<String>,
    pub(super) reexporters: BTreeMap<String, Vec<String>>,
    /// Module keys some `pub use m::*` GLOB re-exports (a plain `pub use m;`
    /// re-exports the module's name, never empties, and isn't one).
    pub(super) glob_reexported_mods: BTreeSet<String>,
    pub(super) foreign_reach: BTreeMap<String, ForeignReach>,
    /// Key → the kinds of test code reaching it (use-sites AND imports) —
    /// see [`TestUser`].
    pub(super) test_users: BTreeMap<String, BTreeSet<TestUser>>,
}

impl EdgeFold {
    pub(super) fn record_test_user(&mut self, key: &str, user: Option<&TestUser>) {
        if let Some(user) = user {
            self.test_users
                .entry(key.to_string())
                .or_default()
                .insert(user.clone());
        }
    }

    /// Count one real use-site of `key` — the single write point of the four
    /// degree maps, so the visibility (cross/intra) and provenance
    /// (test/production) splits can never drift apart.
    pub(super) fn count_use(&mut self, key: String, cross: bool, test_unit: bool) {
        if test_unit {
            *self.test_in_degree.entry(key.clone()).or_insert(0) += 1;
            if !cross {
                *self.test_intra_degree.entry(key.clone()).or_insert(0) += 1;
            }
        }
        *self.in_degree.entry(key.clone()).or_insert(0) += 1;
        if !cross {
            *self.intra_degree.entry(key).or_insert(0) += 1;
        }
    }

    /// Move the removal-sensitive subset out of the fold — the one
    /// [`DegreeMaps`] construction site, shared by the build pass and
    /// [`Assembly::refold_excluding`](super::Assembly::refold_excluding).
    pub(super) fn take_degrees(&mut self) -> DegreeMaps {
        DegreeMaps {
            in_degree: std::mem::take(&mut self.in_degree),
            intra_degree: std::mem::take(&mut self.intra_degree),
            test_in_degree: std::mem::take(&mut self.test_in_degree),
            test_intra_degree: std::mem::take(&mut self.test_intra_degree),
            signature_exposed: std::mem::take(&mut self.signature_exposed),
            foreign_reach: std::mem::take(&mut self.foreign_reach),
            test_users: std::mem::take(&mut self.test_users),
        }
    }
}
