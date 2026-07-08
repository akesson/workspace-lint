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
//! Two more rules (schema 8) extend the model to whole **glob** statements —
//! see `dangling_globs` below:
//!
//! - **Globs are judged by the resolver's own books**: rustc's glob_map
//!   ([`wl_ir::RefEdge::glob_used_names`]) says which names resolved through
//!   each glob (macros, derives, and trait-method resolutions included — the
//!   probe suite pins that). The record is *promiscuous*: method selection
//!   probes every trait candidate a glob supplies, so a real prelude shows
//!   hundreds of recorded names HIR never uses (validated on dioxus's — ~250
//!   names for one `Event` consumer). The accounting therefore weighs only
//!   names with edge evidence: a glob is deleted when removed code demonstrably
//!   leaned on it and no surviving name/identity/trait fact still can. Known
//!   limitation, accepted as fail-open elsewhere: a use visible to the
//!   resolver but not HIR (an intra-doc link) carries no keep-weight.
//! - **Trait-scope facts are a separate evidence class**: typeck's
//!   `used_trait_imports` ([`wl_ir::RefEdge::trait_scope`]) marks the `use`
//!   items method resolution needed in scope. They are never written paths,
//!   so they must not credit leaf imports — but they are the only
//!   survivor-sensitive record of a glob kept alive purely by method syntax.
//!
//! Degradation is direction-safe by construction: an unmodeled construct can
//! only *keep* an import (an `unused_imports` warning for the author), never
//! delete a live one.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::SemanticModel;
use super::assembly::Assembly;
use super::import_usage::ImportTargetUsage;
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
    /// A **glob** (`glob: true`) sets `elem = decl` — the flag, not span
    /// equality, is its discriminator (surgery deletes the whole statement,
    /// or bails on a nested-list glob whose collapsed span isn't one).
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
    /// A whole `use m::*;` statement the removal set orphaned — see the glob
    /// accounting in [`SemanticModel::dangling_imports`]. Surgery deletes the
    /// whole statement (never a leaf excision).
    pub glob: bool,
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
fn scopes_of<S: AsRef<str>>(asm: &Assembly, from: &[S], from_module: &[S]) -> Vec<String> {
    let mut out = Vec::new();
    if from_module.is_empty() {
        for i in (1..=from.len()).rev() {
            let id = wl_ir::join_paths(&from[..i], "::");
            let boundary = i == 1 || (i < from.len() && asm.is_module(&id));
            out.push(id);
            if boundary {
                break;
            }
        }
        return out;
    }
    let module = wl_ir::join_paths(from_module, "::");
    for i in (1..=from.len()).rev() {
        let id = wl_ir::join_paths(&from[..i], "::");
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
    ///   and is the author's to clean, not ours.
    /// - **Globs**: judged whole-statement by the resolver-grounded
    ///   accounting in `dangling_globs` — flagged
    ///   only when every recorded use of the glob is removed and nothing
    ///   surviving could still lean on it.
    ///
    /// `generated_files` (workspace-relative) declares surgery's no-go zone:
    /// an `include!`d file shares its includer's module scope, so a deletion
    /// there can second-order-dangle a `use` *declared in the generated file*
    /// — but editing it would be overwritten by the generator, so such decls
    /// are skipped (first-order danglings can't arise: their targets are
    /// excision-blocked).
    pub fn dangling_imports(
        &self,
        removed: &RemovalSet,
        generated_files: &std::collections::HashSet<PathBuf>,
    ) -> Vec<DanglingImport> {
        use std::collections::HashSet;
        let usage = self.import_target_usage(removed);
        let mut seen: HashSet<(String, u32, u32)> = HashSet::new();
        let mut out = Vec::new();
        for (_, asm) in &self.configs {
            for frag in asm.archived_fragments() {
                for e in frag.references.iter() {
                    if !e.import {
                        continue;
                    }
                    let (Some(decl), Some(elem)) = (e.decl_span.as_ref(), e.elem_span.as_ref())
                    else {
                        continue;
                    };
                    if generated_files.contains(Path::new(decl.file.as_str())) {
                        continue;
                    }
                    // Out-of-workspace targets fall back to the display-path
                    // pseudo-identity `import_target_usage` tracks them under.
                    // They can never be first-order dangling (`removed` holds
                    // workspace identities only) — only the causality-gated
                    // second-order check applies.
                    let pseudo;
                    let id = match asm.target_identity(e) {
                        Some(id) => id,
                        None => {
                            pseudo = wl_ir::join_paths(&e.to, "::");
                            pseudo.as_str()
                        }
                    };
                    let from_crate = e.from.first().map(|s| s.as_str()).unwrap_or_default();
                    let scope = wl_ir::join_paths(&e.from, "::");
                    let dangling = removed.contains_id(id)
                        || (usage.referenced_by_removed(from_crate, &scope, id)
                            && !usage.referenced_by_survivor(from_crate, &scope, id));
                    if !dangling {
                        continue;
                    }
                    if !seen.insert((
                        decl.file.as_str().to_owned(),
                        decl.lo.to_native(),
                        elem.lo.to_native(),
                    )) {
                        continue;
                    }
                    out.push(DanglingImport {
                        decl: wl_ir::Span::from(decl),
                        elem: wl_ir::Span::from(elem),
                        reexport: e.reexport,
                        glob: false,
                    });
                }
            }
        }
        self.dangling_globs(&usage, generated_files, &mut out);
        out
    }

    /// The glob accounting: flag a `use m::*;` whose removal-surviving code
    /// provably no longer needs it. Aggregated per declaration across configs
    /// (the glob_map unions), then judged by rules that each fail toward
    /// *keeping*:
    ///
    /// - **R0** — never a `pub use` re-export, a macro-generated decl, or a
    ///   decl in a generated file.
    /// - **R1 (causality)** — the glob was resolver-alive before the removal
    ///   (`glob_used_names` nonempty, or a `trait_scope` fact reaches it) AND
    ///   some *removed* code accounts for that life. A pre-existing unused
    ///   glob is the author's to clean, exactly like the leaf second-order
    ///   rule.
    /// - **R2 (identity)** — no surviving edge resolves the glob's module (or
    ///   anything under it) at its scope: the favorably-rendered-survivor
    ///   catch.
    /// - **R3 (names)** — no surviving edge's path contains any glob_map name
    ///   at reach of the glob's scope. Rendering-independent: this is what a
    ///   surviving `rsx!` or a divergently-rendered `Element` trips. (The
    ///   probe suite pins that the glob_map records macro, derive, AND
    ///   trait-method resolutions.) Extern-rooted *expansion* paths carry no
    ///   name evidence — a `$crate::…` path bypasses local imports by
    ///   construction, so `tracing::Event` must not shield a glob-supplied
    ///   `Event` (see `import_target_usage`).
    /// - **R4 (trait scope)** — no surviving `trait_scope` fact (typeck's
    ///   `used_trait_imports`) reaches the glob: method syntax whose trait
    ///   the glob supplies.
    /// - **R5 (causality, name half)** — some glob_map name is explained by a
    ///   *removed* edge (the second half of R1). Names with no edge evidence
    ///   at all are typeck probe noise — method selection records every trait
    ///   candidate a glob supplies (~250 on dioxus's prelude for one real
    ///   consumer) — and carry no weight in either direction; real trait
    ///   dependence is R4's precise channel.
    /// - **R6 (belt-and-braces)** — no surviving unattributable external
    ///   resolution in scope (see [`ImportTargetUsage::unattributable`]).
    fn dangling_globs(
        &self,
        usage: &ImportTargetUsage,
        generated_files: &std::collections::HashSet<PathBuf>,
        out: &mut Vec<DanglingImport>,
    ) {
        struct GlobAgg {
            krate: String,
            scope: String,
            target: String,
            used: BTreeSet<String>,
            decl: wl_ir::Span,
        }
        let mut globs: BTreeMap<(String, u32), GlobAgg> = BTreeMap::new();
        for (_, asm) in &self.configs {
            for frag in asm.archived_fragments() {
                for e in frag.references.iter() {
                    if !e.import || !e.glob {
                        continue;
                    }
                    let Some(decl) = e.decl_span.as_ref() else {
                        continue; // macro-generated (or pre-8): no edit surface
                    };
                    if decl.from_expansion
                        || e.reexport
                        || generated_files.contains(Path::new(decl.file.as_str()))
                    {
                        continue; // R0
                    }
                    let target = match asm.target_identity(e) {
                        Some(id) => id.to_string(),
                        None => wl_ir::join_paths(&e.to, "::"),
                    };
                    let agg = globs
                        .entry((decl.file.as_str().to_owned(), decl.lo.to_native()))
                        .or_insert_with(|| GlobAgg {
                            krate: e
                                .from
                                .first()
                                .map(|s| s.as_str())
                                .unwrap_or_default()
                                .into(),
                            scope: wl_ir::join_paths(&e.from, "::"),
                            target,
                            used: BTreeSet::new(),
                            decl: wl_ir::Span::from(decl),
                        });
                    agg.used
                        .extend(e.glob_used_names.iter().map(|n| n.as_str().to_owned()));
                }
            }
        }
        // WL_GLOB_DEBUG=1: print each glob's per-rule verdict — the field
        // instrument that cracked the 2026-07-08 prelude findings.
        let debug = std::env::var_os("WL_GLOB_DEBUG").is_some();
        for g in globs.values() {
            let dbg = |rule: &str| {
                if debug {
                    eprintln!(
                        "WL_GLOB_DEBUG {}:{} target={} scope={} used={:?} => {rule}",
                        g.decl.file, g.decl.line, g.target, g.scope, g.used
                    );
                }
            };
            let trait_removed = usage.trait_scope_reaches(
                &usage.trait_scope_removed,
                &g.krate,
                &g.scope,
                &g.target,
            );
            let trait_survives = usage.trait_scope_reaches(
                &usage.trait_scope_survivor,
                &g.krate,
                &g.scope,
                &g.target,
            );
            // R1 (alive half): a glob the resolver never used is not ours.
            if g.used.is_empty() && !trait_removed && !trait_survives {
                dbg("R1-alive keep");
                continue;
            }
            // R2 / R4: surviving identity or trait-scope evidence.
            if usage.referenced_by_survivor(&g.krate, &g.scope, &g.target) || trait_survives {
                dbg(if trait_survives { "R4 keep" } else { "R2 keep" });
                continue;
            }
            // R3: any surviving use of a glob-supplied name.
            if let Some(n) = g
                .used
                .iter()
                .find(|n| usage.name_in(&usage.survivor_names, &g.krate, &g.scope, n))
            {
                dbg(&format!("R3 keep name={n}"));
                continue;
            }
            // R5 (causality): something removed actually leaned on the glob.
            // Names with NO edge evidence at all (neither survivor nor
            // removed) are typeck resolver probes — the glob_map records
            // every trait candidate touched during method selection, so a
            // real prelude shows hundreds of names HIR never uses. Real
            // trait dependence is covered precisely by the typeck facts
            // (R4); unobservable names carry no keep-weight.
            let caused = trait_removed
                || g.used
                    .iter()
                    .any(|n| usage.name_in(&usage.removed_names, &g.krate, &g.scope, n));
            if !caused {
                dbg("R5-causality keep");
                continue;
            }
            // R6: an unattributable survivor might be resolving through it.
            if usage.unattributable_at(&g.krate, &g.scope) {
                dbg("R6 keep");
                continue;
            }
            dbg("flagged delete");
            out.push(DanglingImport {
                decl: g.decl.clone(),
                elem: g.decl.clone(),
                reexport: false,
                glob: true,
            });
        }
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
            for frag in asm.archived_fragments() {
                for e in frag.references.iter() {
                    let from_crate = e
                        .from
                        .first()
                        .map(|s| s.as_str())
                        .unwrap_or_default()
                        .to_string();
                    // Typeck's used_trait_imports facts: the survivor-
                    // sensitive trait channel. Segregated — never written
                    // resolutions, so they must not enter `by_*` (rule (d):
                    // they'd credit ordinary leaf imports the method call
                    // never resolved through).
                    if e.trait_scope {
                        let id = match asm.target_identity(e) {
                            Some(id) => id.to_string(),
                            None => wl_ir::join_paths(&e.to, "::"),
                        };
                        let set = if removed.covers(&e.from) {
                            &mut usage.trait_scope_removed
                        } else {
                            &mut usage.trait_scope_survivor
                        };
                        for scope in scopes_of(asm, &e.from, &e.from_module) {
                            set.insert((from_crate.clone(), scope, id.clone()));
                        }
                        continue;
                    }
                    if e.import {
                        let scope = wl_ir::join_paths(&e.from, "::");
                        let id = match asm.target_identity(e) {
                            Some(id) => id.to_string(),
                            None => wl_ir::join_paths(&e.to, "::"),
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
                    let is_removed = removed.covers(&e.from);
                    let mut ids: Vec<String> = Vec::new();
                    // NB `def` fields below (`def.path`, `def.trait_item`, …) are
                    // owned `DefInfo` from the assembled index, so they stay
                    // plain `String`; only the raw edge (`e.*`) is archived.
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
                        let id = wl_ir::join_paths(&e.to, "::");
                        // The glob accounting's belt-and-braces (rule R6):
                        // a SURVIVING external resolution the model can't
                        // pin to any import might have come through a glob.
                        // Every class with a precise channel of its own is
                        // exempt: extern-rooted writes bypassed imports;
                        // std/core/alloc are always in the real prelude;
                        // receiver-based calls' trait needs are R4's typeck
                        // facts; macro resolutions are R3's glob_map names
                        // (probe P2); generic params are bound, never
                        // imported. Without the last three, one
                        // `tracing::debug!` R6-blocked every glob in its
                        // module (the 2026-07-08 finding).
                        let root = e.to.first().map(|s| s.as_str()).unwrap_or_default();
                        if !is_removed
                            && !e.extern_root
                            && !e.receiver_resolved
                            && e.to_kind != "macro"
                            && e.to_kind != "param"
                            && !matches!(root, "std" | "core" | "alloc")
                        {
                            for scope in scopes_of(asm, &e.from, &e.from_module) {
                                usage.unattributable.insert((
                                    from_crate.clone(),
                                    scope,
                                    id.clone(),
                                ));
                            }
                        }
                        ids.push(id);
                    }
                    // Name evidence (R3/R5 causality) asks "could this
                    // resolution have gone through a glob?". A macro-expanded
                    // `$crate::…` path (`tracing::debug!` → `tracing::Event`)
                    // is extern-rooted — it bypasses every local import by
                    // construction — so its segments must not shield a glob
                    // that happens to supply a same-named item (dioxus's
                    // `Event` vs tracing's). Written extern-rooted paths stay:
                    // a glob can supply a crate *rename* re-export whose
                    // resolution still reads extern-rooted.
                    let name_blind =
                        e.extern_root && e.span.as_ref().is_some_and(|s| s.from_expansion);
                    let names: Vec<&str> = if name_blind {
                        Vec::new()
                    } else {
                        ids.iter()
                            .flat_map(|id| id.split("::"))
                            .filter(|s| !s.is_empty())
                            .collect()
                    };
                    let (set, name_set) = if is_removed {
                        (&mut usage.by_removed, &mut usage.removed_names)
                    } else {
                        (&mut usage.by_survivor, &mut usage.survivor_names)
                    };
                    for scope in scopes_of(asm, &e.from, &e.from_module) {
                        for id in &ids {
                            set.insert((from_crate.clone(), scope.clone(), id.clone()));
                        }
                        for name in &names {
                            name_set.insert((from_crate.clone(), scope.clone(), name.to_string()));
                        }
                    }
                }
            }
        }
        usage.close_glob_chains();
        usage
    }

    /// Identities that must NOT be auto-deleted because a `use` naming them
    /// can't be excised — deleting the item would dangle an import no edit can
    /// reach (E0432). Two causes, distinguished so the veto note can name the
    /// right one: the declaration lives in a **macro expansion**
    /// (`decl_span.from_expansion`), or in a **generated file** (surgery must
    /// never edit a file its generator will overwrite). The cascade excludes
    /// these from its removable seeds up front, avoiding a mid-run un-delete
    /// ripple. (A `pub use` re-export target is already guarded at the
    /// candidate filter.)
    pub fn import_excision_blocked(
        &self,
        generated_files: &std::collections::HashSet<PathBuf>,
    ) -> std::collections::HashMap<String, ExcisionBlock> {
        let mut out = std::collections::HashMap::new();
        for (_, asm) in &self.configs {
            for frag in asm.archived_fragments() {
                for e in frag.references.iter() {
                    if !e.import {
                        continue;
                    }
                    let Some(decl) = e.decl_span.as_ref() else {
                        continue;
                    };
                    let block = if decl.from_expansion {
                        ExcisionBlock::MacroGenerated
                    } else if generated_files.contains(Path::new(decl.file.as_str())) {
                        ExcisionBlock::GeneratedFile
                    } else {
                        continue;
                    };
                    if let Some(id) = asm.target_identity(e) {
                        // Macro-generated wins on conflict: it's the stricter
                        // claim (no edit surface exists anywhere at all).
                        out.entry(id.to_string())
                            .and_modify(|b| {
                                if block == ExcisionBlock::MacroGenerated {
                                    *b = block;
                                }
                            })
                            .or_insert(block);
                    }
                }
            }
        }
        out
    }
}

/// Why an identity's `use` declarations put it beyond the deletion cascade —
/// see [`SemanticModel::import_excision_blocked`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExcisionBlock {
    /// A `use` naming it is macro-generated: no edit surface exists.
    MacroGenerated,
    /// A `use` naming it lives in a generated (`include!`d) file the
    /// generator owns.
    GeneratedFile,
}
