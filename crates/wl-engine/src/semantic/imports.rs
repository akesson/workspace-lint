//! The dangling-import surface: which `use` leaves a cascade deletion strands.
//!
//! The second-order check is a miniature model of rustc's import resolution,
//! because that is what `unused_imports` judges: a `use` statement is *used*
//! iff some source-level name resolution went through it. Four consequences
//! shape everything here (each learned from a delete-mode residue class on
//! the 2026-07-05/06 LeaveDates validation):
//!
//! - **Scope is the enclosing module**, not the crate or the file: a nested
//!   `#[cfg(test)] mod tests` with its own `use crate::Date;` resolves through
//!   *that* import, so its uses cannot keep the outer file-top import alive.
//! - **Glob re-imports bridge modules**: a test module with `use super::*`
//!   (and no explicit import of the name) resolves through the parent's
//!   import — deleting the parent leaf would break the child. Explicit beats
//!   glob: if the child *also* imports the name explicitly, the parent leaf
//!   is not reached (rustc prefers the explicit binding).
//! - **Only source name-resolutions count**: the lowered-signature pass emits
//!   span-less edges for *normalized* types (`GlobalSignal<T>` reaching
//!   `Signal`) that the source never names — they must not shield an import.
//!   Every name actually written in source has its own spanned `visit_path`
//!   edge, so dropping the lowered ones loses nothing.
//! - **Receiver-based resolutions bypass imports**: an inherent `.time()`
//!   call or `x.field` read resolves from the receiver's type
//!   ([`wl_ir::RefEdge::receiver_resolved`]), so it cannot keep
//!   `use …::TimeView;` alive. Trait members are the exception — the trait
//!   must be in scope for the call to resolve, so they credit the trait's
//!   import regardless.
//!
//! Degradation is direction-safe by construction: an unmodeled construct can
//! only *keep* an import (an `unused_imports` warning for the author), never
//! delete a live one.

use std::collections::{BTreeMap, BTreeSet};

use super::SemanticModel;
use super::assembly::Assembly;
use super::removal::RemovalSet;

/// One `use`-declaration leaf left dangling by a cascade deletion: its target
/// item is being removed, so the import must go too or the build breaks
/// (E0432). The unused-pub `--fix` import-surgery surface
/// ([`SemanticModel::dangling_imports`]).
#[derive(Debug, Clone)]
pub struct DanglingImport {
    /// The leaf item's own span (workspace-relative file, on-disk byte range).
    /// For a **standalone** `use a::b;` this is the whole statement — the
    /// delete surface. For a **brace-list** leaf rustc collapses it to the
    /// leaf, so it equals `elem`; that equality is the brace discriminator the
    /// lint keys on (`decl == elem` ⇒ excise in place, else delete statement).
    pub decl: wl_ir::Span,
    /// The leaf as written (`b`, `b::c`, or `B as C`) — the intra-brace
    /// excision surface. Covers the whole brace entry, nested path and rename
    /// included.
    pub elem: wl_ir::Span,
    /// A `pub use` re-export: excising it can break a downstream name
    /// (E0364/E0365). Surgery skips it — and the target should never be a
    /// deletion candidate anyway (the candidate filter guards re-export
    /// targets), so this is belt-and-braces.
    pub reexport: bool,
}

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
struct ImportTargetUsage {
    by_survivor: BTreeSet<(String, String, String)>,
    by_removed: BTreeSet<(String, String, String)>,
    /// `(crate, module M)` → modules whose glob imports reach M
    /// (`use super::*` chains, transitively closed).
    glob_importers: BTreeMap<(String, String), BTreeSet<String>>,
    /// `(crate, module, identity)`: the module has its own explicit (non-glob)
    /// import of the identity, so its references resolve *there* and never
    /// through a glob into another module's import.
    own_imports: BTreeSet<(String, String, String)>,
}

impl ImportTargetUsage {
    fn referenced_by_survivor(&self, krate: &str, scope: &str, id: &str) -> bool {
        self.reaches(&self.by_survivor, krate, scope, id)
    }

    fn referenced_by_removed(&self, krate: &str, scope: &str, id: &str) -> bool {
        self.reaches(&self.by_removed, krate, scope, id)
    }

    /// Does some reference in `set` resolve `id` through an import at `scope`?
    /// Scope-exact, or from a module whose glob chain reaches `scope` and that
    /// has no explicit import of `id` itself.
    fn reaches(
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

    fn probe(set: &BTreeSet<(String, String, String)>, krate: &str, scope: &str, id: &str) -> bool {
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

    /// Transitively close the glob graph: `P` globs `N`, `N` globs `M` ⇒ a
    /// reference in `P` can reach `M`'s imports. Glob graphs are tiny (`use
    /// super::*` in test modules, the odd prelude), so a plain fixpoint does.
    fn close_glob_chains(&mut self) {
        loop {
            let snapshot = self.glob_importers.clone();
            let mut grew = false;
            // For every (crate, M) → {N}, add each N's own glob-importers.
            for ((krate, m), importers) in &snapshot {
                let mut add: BTreeSet<String> = BTreeSet::new();
                for n in importers {
                    if let Some(pp) = snapshot.get(&(krate.clone(), n.clone())) {
                        add.extend(pp.iter().cloned());
                    }
                }
                if !add.is_empty() {
                    let entry = self
                        .glob_importers
                        .get_mut(&(krate.clone(), m.clone()))
                        .expect("key came from a snapshot of this map");
                    let before = entry.len();
                    entry.extend(add);
                    grew |= entry.len() > before;
                }
            }
            if !grew {
                return;
            }
        }
    }
}

/// The scopes a reference resolves imports in: every prefix of its enclosing
/// item's def path, longest first, down to and including the nearest enclosing
/// module (or the crate root). Stops there — a `use` in a parent module is a
/// different scope; nested modules only reach it via glob re-imports.
///
/// The module terminal is the edge's **lexical** [`wl_ir::RefEdge::from_module`],
/// not a textual prefix: `def_path_str` renders an impl member at its
/// self-type's path (`Type::method`, `<Type as Trait>::method`), which hides
/// the real module entirely (trait impls — no prefix is a module, so the walk
/// used to fall through to the crate root and the body's trait-method calls
/// credited no module's imports) or names a *different* module's chain (an
/// inherent impl written outside its type's module). rustc resolves a body's
/// names through the imports of the module the code is lexically in, so the
/// prefix walk stops at the first textual module boundary and the lexical
/// module is appended in its place. Pre-6 fragments (empty `from_module`)
/// keep the historical textual walk.
fn scopes_of(asm: &Assembly, from: &[String], from_module: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    if from_module.is_empty() {
        for i in (1..=from.len()).rev() {
            let id = from[..i].join("::");
            let boundary = i == 1 || (i < from.len() && asm.is_module(&id));
            out.push(id);
            if boundary {
                break;
            }
        }
        return out;
    }
    let module = from_module.join("::");
    for i in (1..=from.len()).rev() {
        let id = from[..i].join("::");
        if id == module || i == 1 || (i < from.len() && asm.is_module(&id)) {
            break;
        }
        out.push(id);
    }
    out.push(module);
    out
}

impl SemanticModel {
    /// Every `use`-declaration leaf a cascade deletion leaves dangling.
    /// Two orders, unioned across configs, deduped by (file, decl, elem) —
    /// the unused-pub `--fix` import-surgery surface. Macro-generated and
    /// `pub use` leaves are surfaced but flagged for the lint to skip.
    ///
    /// - **First order**: the import's target itself is being removed —
    ///   keeping the `use` is E0432.
    /// - **Second order**: the target survives but every source reference
    ///   that resolved through this `use` (module-scoped — see the module
    ///   docs) was removed — keeping it is an `unused_imports` warning, a
    ///   `-D warnings` failure. Flagged only when some *removed* def was such
    ///   a user (the deletion caused the danglingness): a pre-existing unused
    ///   import may have users in cfg universes the engine never extracts,
    ///   and is the author's to clean, not ours. Glob imports themselves are
    ///   never flagged (no leaf span to excise; rustc will point at them).
    pub fn dangling_imports(&self, removed: &RemovalSet) -> Vec<DanglingImport> {
        use std::collections::HashSet;
        let usage = self.import_target_usage(removed);
        let mut seen: HashSet<(String, u32, u32)> = HashSet::new();
        let mut out = Vec::new();
        for (_, asm) in &self.configs {
            for frag in asm.fragments() {
                for e in &frag.references {
                    if !e.import {
                        continue;
                    }
                    let (Some(decl), Some(elem)) = (&e.decl_span, &e.elem_span) else {
                        continue;
                    };
                    // Out-of-workspace targets fall back to the display-path
                    // pseudo-identity `import_target_usage` tracks them under.
                    // They can never be first-order dangling (`removed` holds
                    // workspace identities only) — only the causality-gated
                    // second-order check applies.
                    let pseudo;
                    let id = match asm.target_identity(e) {
                        Some(id) => id,
                        None => {
                            pseudo = e.to.join("::");
                            pseudo.as_str()
                        }
                    };
                    let from_crate = e.from.first().map(String::as_str).unwrap_or_default();
                    let scope = e.from.join("::");
                    let dangling = removed.contains_id(id)
                        || (usage.referenced_by_removed(from_crate, &scope, id)
                            && !usage.referenced_by_survivor(from_crate, &scope, id));
                    if !dangling {
                        continue;
                    }
                    if !seen.insert((decl.file.clone(), decl.lo, elem.lo)) {
                        continue;
                    }
                    out.push(DanglingImport {
                        decl: decl.clone(),
                        elem: elem.clone(),
                        reexport: e.reexport,
                    });
                }
            }
        }
        out
    }

    /// The per-crate, per-scope identity-usage split behind the second-order
    /// dangling check — see [`ImportTargetUsage`]. Trait-impl targets also
    /// credit the implemented trait *member*'s identity (whose path is
    /// prefixed by the trait's), so a trait import kept alive by method calls
    /// through a blanket impl in a third crate still counts; inherent-impl
    /// targets credit their nominal self type the same way (a remote impl
    /// renders at the impl's module, off the type's prefix). Targets outside
    /// the workspace are tracked under their display path — the
    /// pseudo-identity that lets imports of third-party items dangle too.
    fn import_target_usage(&self, removed: &RemovalSet) -> ImportTargetUsage {
        let mut usage = ImportTargetUsage::default();
        for (_, asm) in &self.configs {
            for frag in asm.fragments() {
                for e in &frag.references {
                    let from_crate = e
                        .from
                        .first()
                        .map(String::as_str)
                        .unwrap_or_default()
                        .to_string();
                    if e.import {
                        let scope = e.from.join("::");
                        let id = match asm.target_identity(e) {
                            Some(id) => id.to_string(),
                            None => e.to.join("::"),
                        };
                        if e.glob {
                            // `use m::*` at `scope`: references in `scope` can
                            // resolve through imports in module `id`.
                            usage
                                .glob_importers
                                .entry((from_crate, id))
                                .or_default()
                                .insert(scope);
                        } else {
                            usage.own_imports.insert((from_crate, scope, id));
                        }
                        continue;
                    }
                    // Lowered-signature edges are normalized-type projections,
                    // not source name-resolutions — a name written in source
                    // always has its own spanned `visit_path` edge. Keeping
                    // them would phantom-shield imports of names the source
                    // never writes (`GlobalSignal<T>` reaching `Signal`).
                    if e.in_signature {
                        continue;
                    }
                    let set = if removed.covers(&e.from) {
                        &mut usage.by_removed
                    } else {
                        &mut usage.by_survivor
                    };
                    let mut ids: Vec<String> = Vec::new();
                    if let Some(key) = asm.resolve_key(e)
                        && let Some(def) = asm.defs.get(key)
                    {
                        // A receiver-based resolution (`.time()`, `x.field`)
                        // involves no written path, so it never resolves
                        // through the type's import — crediting it would
                        // shield a `use …::TimeView;` rustc reports unused.
                        // Trait members are the exception: the trait must be
                        // in scope for the call to resolve at all, so they
                        // credit the trait's import regardless.
                        let trait_member =
                            def.trait_item.is_some() || asm.trait_parent.contains_key(key);
                        if !e.receiver_resolved || trait_member {
                            ids.push(def.path.clone());
                        }
                        if let Some(ti) = &def.trait_item
                            && let Some(tm) = asm.defs.get(ti)
                        {
                            ids.push(tm.path.clone());
                        }
                        // A written `Type::assoc` path on a remote impl
                        // credits its nominal self type: `def_path_str`
                        // renders a remote impl at the *impl's* module
                        // (`m::<impl crate::Type>::method`), so the prefix
                        // probe can never reach an import of `Type` through
                        // the member's own path.
                        if !e.receiver_resolved
                            && let Some(st) = &def.self_type
                            && let Some(td) = asm.defs.get(st)
                        {
                            ids.push(td.path.clone());
                        }
                    } else if let Some(id) = asm.target_identity(e) {
                        ids.push(id.to_string());
                    } else {
                        // Out-of-workspace target (std/third-party): no def,
                        // no identity — track it by display path so an import
                        // of e.g. `anyhow::Context` still reads as dangling
                        // when its last user is deleted (the pseudo-identity
                        // the dangling check falls back to). Kept for
                        // receiver-based edges too: an external trait method
                        // (`.context()`) is indistinguishable from an
                        // inherent one out here, and under-crediting would
                        // delete a live trait import.
                        ids.push(e.to.join("::"));
                    }
                    for scope in scopes_of(asm, &e.from, &e.from_module) {
                        for id in &ids {
                            set.insert((from_crate.clone(), scope.clone(), id.clone()));
                        }
                    }
                }
            }
        }
        usage.close_glob_chains();
        usage
    }

    /// Identities that must NOT be auto-deleted because a `use` naming them
    /// can't be excised: the declaration lives in a **macro expansion**
    /// (`decl_span.from_expansion`), so deleting the item would dangle an
    /// import no edit can reach (E0432). The cascade excludes these from its
    /// removable seeds up front, avoiding a mid-run un-delete ripple. (A
    /// `pub use` re-export target is already guarded at the candidate filter.)
    pub fn import_excision_blocked(&self) -> std::collections::HashSet<String> {
        let mut out = std::collections::HashSet::new();
        for (_, asm) in &self.configs {
            for frag in asm.fragments() {
                for e in &frag.references {
                    if !e.import {
                        continue;
                    }
                    let blocked = e.decl_span.as_ref().is_some_and(|d| d.from_expansion);
                    if blocked && let Some(id) = asm.target_identity(e) {
                        out.insert(id.to_string());
                    }
                }
            }
        }
        out
    }
}
