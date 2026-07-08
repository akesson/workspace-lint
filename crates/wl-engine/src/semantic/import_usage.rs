//! [`ImportTargetUsage`]: the scope-indexed evidence sets behind the
//! dangling-import derivation, and their resolution-model queries. The model
//! itself (what goes INTO the sets, and the rules that judge them) lives in
//! [`imports`](super::imports) — this module is the substrate: prefix-aware
//! probes, glob-chain reach, explicit-beats-glob, and name/trait/
//! unattributable evidence lookups.

use std::collections::{BTreeMap, BTreeSet};

/// `(crate, scope, identity)` sets of what each scope's real edges reference,
/// split removed/surviving, plus the two resolution substrates the scope
/// query needs: which modules reach which through glob re-imports, and which
/// modules explicitly import which identities (explicit-beats-glob).
///
/// A *scope* is a def-path prefix, `::`-joined. References are indexed under
/// their whole enclosing-item chain down to the nearest enclosing module (so
/// an import written inside a fn is served by that fn's own references, and a
/// module-scope import by everything directly in the module) — but never
/// across a module boundary, which is exactly the reach of a `use` statement.
///
/// Queries are prefix-aware — a reference to `a::StrExt::shout` counts as a
/// use of an imported `a::StrExt` (method calls never name the trait itself).
#[derive(Default)]
pub(super) struct ImportTargetUsage {
    pub(super) by_survivor: BTreeSet<(String, String, String)>,
    pub(super) by_removed: BTreeSet<(String, String, String)>,
    /// `(crate, module M)` → modules whose glob imports reach M
    /// (`use super::*` chains, transitively closed).
    pub(super) glob_importers: BTreeMap<(String, String), BTreeSet<String>>,
    /// `(crate, module, identity)`: the module has its own explicit (non-glob)
    /// import of the identity, so its references resolve *there* and never
    /// through a glob into another module's import.
    pub(super) own_imports: BTreeSet<(String, String, String)>,
    /// `(crate, scope, segment)` — every path segment of every identity in
    /// `by_survivor` / `by_removed`. The rendering-independent name evidence
    /// of the glob accounting: a survivor rendered `dioxus_core::Element` and
    /// a glob targeting `dioxus::prelude` disagree on every path prefix, but
    /// never on the segment `Element` the glob_map recorded.
    pub(super) survivor_names: BTreeSet<(String, String, String)>,
    pub(super) removed_names: BTreeSet<(String, String, String)>,
    /// `(crate, scope, target-identity)` from [`wl_ir::RefEdge::trait_scope`]
    /// facts — "a body at `scope` needed this `use` item's target in scope
    /// for method resolution". Kept OUT of `by_*`: not written resolutions
    /// (they must never credit an ordinary leaf import), but the
    /// survivor-sensitive trait channel of the glob accounting.
    pub(super) trait_scope_survivor: BTreeSet<(String, String, String)>,
    pub(super) trait_scope_removed: BTreeSet<(String, String, String)>,
    /// `(crate, scope, identity)` of surviving edges no import could be shown
    /// to explain — external-unresolved targets (no workspace def) that are
    /// neither extern-rooted (written path bypassed imports), std/core/alloc-
    /// rooted, nor receiver-resolved-and-non-trait. The glob accounting's
    /// belt-and-braces: such a survivor *might* resolve through any glob in
    /// scope, so no glob it can reach is deleted (own-import coverage is
    /// checked at query time — explicit-beats-glob).
    pub(super) unattributable: BTreeSet<(String, String, String)>,
}

impl ImportTargetUsage {
    /// Does some reference in `set` (`by_survivor` / `by_removed`) resolve
    /// `id` through an import at `scope`?
    /// Scope-exact, or from a module whose glob chain reaches `scope` and that
    /// has no explicit import of `id` itself.
    pub(super) fn reaches(
        &self,
        set: &BTreeSet<(String, String, String)>,
        krate: &str,
        scope: &str,
        id: &str,
    ) -> bool {
        if Self::probe(set, krate, scope, id) {
            return true;
        }
        let Some(globbers) = self
            .glob_importers
            .get(&(krate.to_string(), scope.to_string()))
        else {
            return false;
        };
        globbers.iter().any(|n| {
            Self::probe(set, krate, n, id) && !Self::probe(&self.own_imports, krate, n, id)
        })
    }

    pub(super) fn probe(
        set: &BTreeSet<(String, String, String)>,
        krate: &str,
        scope: &str,
        id: &str,
    ) -> bool {
        let key = (krate.to_string(), scope.to_string(), id.to_string());
        if set.contains(&key) {
            return true;
        }
        // Any identity under `id::` (assoc items, module children) keeps the
        // import of `id` alive — one ordered range probe.
        let prefix = format!("{id}::");
        set.range((krate.to_string(), scope.to_string(), prefix.clone())..)
            .next()
            .is_some_and(|(k, s, i)| k == krate && s == scope && i.starts_with(&prefix))
    }

    /// Does an edge in `set` (`survivor_names` / `removed_names`) use `name`
    /// — a bare path segment — at `scope` or a scope whose glob chain reaches
    /// it? Exact-segment membership, the name twin of
    /// [`reaches`](Self::reaches). No own-import subtraction: skipping it
    /// only ever *keeps* a glob (the safe direction — a survivor using the
    /// name through its own leaf import shields the glob it didn't need).
    pub(super) fn name_in(
        &self,
        set: &BTreeSet<(String, String, String)>,
        krate: &str,
        scope: &str,
        name: &str,
    ) -> bool {
        let key = |s: &str| (krate.to_string(), s.to_string(), name.to_string());
        if set.contains(&key(scope)) {
            return true;
        }
        self.glob_importers
            .get(&(krate.to_string(), scope.to_string()))
            .is_some_and(|gs| gs.iter().any(|s| set.contains(&key(s))))
    }

    /// Does a `trait_scope` fact in `set` reach the glob at `(krate, scope)`
    /// targeting `id`? Scope-exact or over the glob chain, like
    /// [`reaches`](Self::reaches).
    pub(super) fn trait_scope_reaches(
        &self,
        set: &BTreeSet<(String, String, String)>,
        krate: &str,
        scope: &str,
        id: &str,
    ) -> bool {
        if Self::probe(set, krate, scope, id) {
            return true;
        }
        self.glob_importers
            .get(&(krate.to_string(), scope.to_string()))
            .is_some_and(|gs| gs.iter().any(|s| Self::probe(set, krate, s, id)))
    }

    /// Is there a surviving edge at reach of `scope` whose resolution no
    /// import can be shown to explain (and that an explicit own import does
    /// not cover)? While one exists, no glob reachable from that code may be
    /// deleted — the accounting can't prove the glob wasn't its supplier.
    pub(super) fn unattributable_at(&self, krate: &str, scope: &str) -> bool {
        let scopes = std::iter::once(scope.to_string()).chain(
            self.glob_importers
                .get(&(krate.to_string(), scope.to_string()))
                .into_iter()
                .flatten()
                .cloned(),
        );
        for s in scopes {
            let lo = (krate.to_string(), s.clone(), String::new());
            for (k, sc, id) in self.unattributable.range(lo..) {
                if k != krate || *sc != s {
                    break;
                }
                if !self.own_covers(krate, &s, id) {
                    return true;
                }
            }
        }
        false
    }

    /// Explicit-beats-glob for the unattributable probe: is some segment-wise
    /// prefix of `id` explicitly imported at `scope`? (`use js_sys::RegExp;`
    /// covers a survivor edge to `js_sys::RegExp::test`.)
    pub(super) fn own_covers(&self, krate: &str, scope: &str, id: &str) -> bool {
        let mut end = id.len();
        loop {
            let prefix = &id[..end];
            if self.own_imports.contains(&(
                krate.to_string(),
                scope.to_string(),
                prefix.to_string(),
            )) {
                return true;
            }
            match prefix.rfind("::") {
                Some(p) => end = p,
                None => return false,
            }
        }
    }

    /// Transitively close the glob graph: `P` globs `N`, `N` globs `M` ⇒ a
    /// reference in `P` can reach `M`'s imports. Glob graphs are tiny (`use
    /// super::*` in test modules, the odd prelude), so a plain fixpoint does.
    pub(super) fn close_glob_chains(&mut self) {
        let mut grew = true;
        while grew {
            grew = false;
            let snapshot = self.glob_importers.clone();
            // For every (crate, M) → {N}, add each N's own glob-importers.
            for ((krate, _), importers) in &mut self.glob_importers {
                let add: Vec<String> = importers
                    .iter()
                    .filter_map(|n| snapshot.get(&(krate.clone(), n.clone())))
                    .flat_map(|pp| pp.iter().cloned())
                    .collect();
                let before = importers.len();
                importers.extend(add);
                grew |= importers.len() > before;
            }
        }
    }
}
