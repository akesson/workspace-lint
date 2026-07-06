//! The unused-pub `--fix` cascade's removal vocabulary: which defs to treat
//! as deleted ([`RemovalSet`]), the indexes the deletion invalidates and
//! [`super::Assembly::refold_excluding`] recomputes ([`RemovalOverlay`]), and
//! the borrowed view ([`DegreeView`]) that lets the verdict fold read either
//! the prebuilt or the recomputed maps through one interface.

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

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Exact identity membership — the import index asks "is *this* import's
    /// target one of the removed defs?" (a dangling import names its target
    /// exactly, not an ancestor).
    pub fn contains_id(&self, id: &str) -> bool {
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

/// The removal-sensitive indexes recomputed by
/// [`super::Assembly::refold_excluding`] — the degree source
/// [`super::pub_usage::compute`] reads instead of the prebuilt maps when a
/// cascade removal set is in effect.
pub(super) struct RemovalOverlay {
    pub(super) in_degree: BTreeMap<String, usize>,
    pub(super) intra_degree: BTreeMap<String, usize>,
    pub(super) signature_exposed: BTreeSet<String>,
    pub(super) foreign_reach: BTreeMap<String, ForeignReach>,
}

impl RemovalOverlay {
    pub(super) fn view(&self) -> DegreeView<'_> {
        DegreeView {
            in_degree: &self.in_degree,
            intra_degree: &self.intra_degree,
            signature_exposed: &self.signature_exposed,
            foreign_reach: &self.foreign_reach,
        }
    }
}

/// A borrowed view of the removal-sensitive indexes — either the prebuilt
/// maps ([`super::Assembly::degree_view`]) or a recomputed [`RemovalOverlay`].
/// The degree source [`super::pub_usage::compute`] reads, so the same fold
/// serves both the plain and the cascade paths.
pub(super) struct DegreeView<'a> {
    pub(super) in_degree: &'a BTreeMap<String, usize>,
    pub(super) intra_degree: &'a BTreeMap<String, usize>,
    pub(super) signature_exposed: &'a BTreeSet<String>,
    pub(super) foreign_reach: &'a BTreeMap<String, ForeignReach>,
}
