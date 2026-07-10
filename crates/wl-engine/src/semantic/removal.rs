//! The unused-pub `--fix` cascade's removal vocabulary: which defs to treat
//! as deleted ([`RemovalSet`]), the removal-sensitive index bundle
//! ([`DegreeMaps`] — held prebuilt by `Assembly` and recomputed by
//! [`super::Assembly::refold_excluding`]), and the borrowed view
//! ([`DegreeView`]) that lets the verdict fold read either copy through one
//! interface.

use std::collections::{BTreeMap, BTreeSet};

use super::join::ForeignReach;

/// A set of def identities (crate-rooted display paths) to treat as deleted
/// when the unused-pub `--fix` cascade recomputes degrees. Matching is
/// **segment-wise prefix**, not string prefix, so removing `crate::a` drops
/// `crate::a`'s own edges and those of body-nested defs it owns
/// (`crate::a::{closure}`) but never a sibling `crate::ab`.
#[derive(Default)]
pub struct RemovalSet {
    segs: Vec<Vec<String>>,
    ids: std::collections::HashSet<String>,
}

impl RemovalSet {
    /// Build from cross-config identities (`PubCandidate::id` / `DefInfo::path`).
    pub fn new<I, S>(ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let ids: std::collections::HashSet<String> =
            ids.into_iter().map(|id| id.as_ref().to_string()).collect();
        let segs = ids
            .iter()
            .map(|id| id.split("::").map(str::to_string).collect())
            .collect();
        Self { segs, ids }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Exact identity membership — the import index asks "is *this* import's
    /// target one of the removed defs?" (a dangling import names its target
    /// exactly, not an ancestor).
    pub(crate) fn contains_id(&self, id: &str) -> bool {
        self.ids.contains(id)
    }

    /// Does some removed identity equal `from` or a proper ancestor of it
    /// (segment-wise)? `from` is an edge's enclosing-item path. Generic over
    /// `AsRef<str>` so the archived runtime (`&[ArchivedString]`) and native
    /// unit tests (`&[String]`) share one body.
    pub(super) fn covers<S: AsRef<str>>(&self, from: &[S]) -> bool {
        self.segs.iter().any(|r| {
            from.len() >= r.len() && r.iter().zip(from).all(|(a, b)| a.as_str() == b.as_ref())
        })
    }
}

/// The removal-sensitive indexes, bundled: the maps that change when defs
/// are treated as deleted, and nothing else. `Assembly` holds the prebuilt
/// copy; [`super::Assembly::refold_excluding`] recomputes one for the
/// unused-pub `--fix` cascade. One owned type (instead of parallel fields in
/// each holder) so the [`Self::view`] projection exists exactly once.
#[derive(Default)]
pub(super) struct DegreeMaps {
    /// Stable key → **workspace-wide** in-degree of real (non-import)
    /// use-sites. The reverse index.
    pub(super) in_degree: BTreeMap<String, usize>,
    /// Stable key → intra-crate-only in-degree (kept for reporting deltas).
    pub(super) intra_degree: BTreeMap<String, usize>,
    /// The subset of `in_degree` contributed by test units (`+test` cfg
    /// variants, integration tests, benches). `in_degree - test_in_degree`
    /// is production reach — the test-only classification's substrate.
    pub(super) test_in_degree: BTreeMap<String, usize>,
    /// The subset of `intra_degree` contributed by test units.
    pub(super) test_intra_degree: BTreeMap<String, usize>,
    /// Keys named in some PUB item's signature (`in_signature` edges whose
    /// `from` def is public) — the `exposed_in_public_signature` substrate.
    pub(super) signature_exposed: BTreeSet<String>,
    /// Reach credited to identities the config has **no def for** (the
    /// defining crate wasn't extracted here at all). See [`ForeignReach`].
    pub(super) foreign_reach: BTreeMap<String, ForeignReach>,
}

impl DegreeMaps {
    pub(super) fn view(&self) -> DegreeView<'_> {
        DegreeView {
            in_degree: &self.in_degree,
            intra_degree: &self.intra_degree,
            test_in_degree: &self.test_in_degree,
            test_intra_degree: &self.test_intra_degree,
            signature_exposed: &self.signature_exposed,
            foreign_reach: &self.foreign_reach,
        }
    }
}

/// A borrowed view of the removal-sensitive indexes — either the prebuilt
/// [`DegreeMaps`] or a recomputed one. The degree source
/// [`super::pub_usage::compute`] reads, so the same fold serves both the
/// plain and the cascade paths.
pub(super) struct DegreeView<'a> {
    pub(super) in_degree: &'a BTreeMap<String, usize>,
    pub(super) intra_degree: &'a BTreeMap<String, usize>,
    pub(super) test_in_degree: &'a BTreeMap<String, usize>,
    pub(super) test_intra_degree: &'a BTreeMap<String, usize>,
    pub(super) signature_exposed: &'a BTreeSet<String>,
    pub(super) foreign_reach: &'a BTreeMap<String, ForeignReach>,
}
