//! The single-config assembly: the multi-crate join (SPIKE §4/§5).
//!
//! Joins every per-crate `IrFragment` into a workspace-global def index and a
//! **cross-crate reverse index**, both keyed by the stable `DefPathHash`
//! (`ItemFact::key` / `RefEdge::to_key`) — **not** the display path:
//! `def_path_str` renders a def at its definition path in the defining crate
//! but at its re-export path in a consumer, so a path-equality join scores
//! 0/215 cross-crate (see `wl_ir::RefEdge`). Lifted from the spike assembler.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use foldhash::fast::FixedState;
use indexmap::IndexMap;
use wl_ir::{ArchivedIrFragment, ArchivedRefEdge, Visibility};

pub(super) use super::def_info::{CANDIDATE_KINDS, CandReach};
pub use super::def_info::{Category, DefInfo, Reach, ResolvedRef};

/// The global def index: `DefPathHash` key → the def it names. **Insertion
/// ordered** (an `IndexMap`), so iteration is deterministic — the derived
/// clippy-guard indexes and the cfg-matrix union walk it — while lookups are
/// O(1) fast-hash rather than the `BTreeMap<String>` string-compare over ~100k
/// keys the ~1.65M-edge fold used to pay per edge. Insertion order is fixed by
/// [`super::load_fragments`]' fragment sort × the extractor's item order, so it
/// is stable across runs (the fixed-seed hasher never reorders it).
pub(super) type DefMap = IndexMap<String, DefInfo, FixedState>;

/// A fast-hash lookup map for the join's other hot, **iteration-order-agnostic**
/// keys (`DefPathHash` / display path → key). Fixed seed ⇒ reproducible; only
/// ever `get`/`insert`, never iterated in an output-visible order.
pub(super) type FastMap<K, V> = std::collections::HashMap<K, V, FixedState>;
use super::join::{ForeignReach, IdentityIndex};
use super::meta::WorkspaceMeta;
use super::removal::{DegreeMaps, DegreeView, RemovalSet};
use super::store::FragmentBytes;

/// One config's assembled workspace: fragments plus the derived global
/// indexes — the driver-backed replacement for the resolver's global view.
pub struct Assembly {
    fragments: Vec<FragmentBytes>,
    /// Every fragment crate's code-form name.
    pub(super) crates: BTreeSet<String>,
    /// Stable key → the def it identifies. The global symbol table.
    pub(super) defs: DefMap,
    /// The prebuilt removal-sensitive index bundle (`in_degree`,
    /// `intra_degree`, `signature_exposed`, `foreign_reach`) — see
    /// [`DegreeMaps`]. `pub(super)` so the union/collateral folds can read
    /// `foreign_reach` alongside the recomputed overlays.
    pub(super) degrees: DegreeMaps,
    /// (from-crate, defining-crate) → edge count, workspace-internal only,
    /// counting ALL edges (imports included — importing B's item uses B).
    pub(super) dep_matrix: BTreeMap<(String, String), usize>,
    /// Crates referenced by *some other* fragment crate (the dependency-leaf
    /// proxy — the boundary fallback when no metadata is available).
    referenced: BTreeSet<String>,
    /// `use`/re-export edges discounted from the reverse index.
    pub(super) import_edges: usize,
    /// Trait-item key → the impl-item keys implementing it (the dispatch
    /// linkage: a dispatched trait method reaches every impl of it).
    impls_of: BTreeMap<String, Vec<String>>,
    /// Trait-*member* key → the owning trait declaration's key. A method-call
    /// edge resolves to the member def (`parent_kind == "trait"`, never a
    /// candidate); the trait itself is only reachable through its members at
    /// most call sites, so the fold credits every member edge to the parent
    /// trait too (an extension trait imported cross-crate purely for method
    /// syntax must stay `pub`).
    pub(super) trait_parent: FastMap<String, String>,
    /// Owner key (inherent-impl self type, or trait declaration) → its assoc
    /// **fn** member keys — the member enumeration behind the clippy-unmask
    /// guard (`super::clippy_guard`). Trait-*impl* methods are absent by
    /// construction (their conventions are the trait's, and clippy skips
    /// them too).
    pub(super) assoc_members: BTreeMap<String, Vec<String>>,
    /// ADT key → its field keys (underscore-prefixed fields excluded, as
    /// rustc `dead_code` excludes them) — the dead-field narrow guard's
    /// enumeration ([`Assembly::has_unread_field`]).
    pub(super) fields_of: BTreeMap<String, Vec<String>>,
    /// Keys that are the target of a `pub use` re-export
    /// (`RefEdge::reexport`). Tightening or deleting such a def can break the
    /// re-export (E0364/E0365), so the unused-pub port suppresses these — the
    /// driver-backed analog of the syn re-export-index `is_target` guard.
    import_targets: BTreeSet<String>,
    /// Re-export-target key → the modules whose `pub use` declarations name
    /// it (the edge's `from` — `visit_use` attributes a use-path to its
    /// enclosing module). The re-export leg of external reachability: a def
    /// `pub use`d in an externally-reachable module is nameable through it.
    reexporters: BTreeMap<String, Vec<String>>,
    /// `mod` def path (joined) → is-public, for the pub-module-hop
    /// reachability judgement.
    module_vis: BTreeMap<String, bool>,
    /// Display-path → stable key, over lib/proc-macro fragments only — the
    /// build-fragment edge join fallback (see the fold in [`Assembly::build`]).
    /// Retained so [`Assembly::refold_excluding`] can re-run the same join when
    /// the unused-pub `--fix` cascade recomputes degrees with items removed.
    path_key: FastMap<String, String>,
    /// (crate, item name) → the crate's defs carrying that trailing segment —
    /// the suffix-relaxed leg of the path fallback: an edge into a
    /// feature-shifted generation NO config extracted renders its target at
    /// the *re-export* path (`data_common::Fraction::new`), which exact path
    /// equality can't join to the definition path
    /// (`data_common::fraction::Fraction::new`). Same index domain as
    /// `path_key`.
    name_key: FastMap<(String, String), Vec<(String, String)>>,
    /// The global (all-configs) key → identity index, for resolving an edge
    /// whose target was extracted only under another config (see
    /// [`Assembly::resolve_key`]).
    ids: Arc<IdentityIndex>,
    /// Cross-config identity → **this** config's key for it — the landing half
    /// of the global join (mirrors `path_key`'s shape). A global-index hit is
    /// translated through here to a local def before its use-site is counted,
    /// so every degree stays keyed by a def in `defs`. First fragment wins;
    /// deterministic because `load_fragments` sorts.
    id_key: FastMap<String, String>,
}

/// The edge-derived indexes produced by one pass of
/// [`Assembly::fold_edges`] — the reverse index the reachability verdicts read.
#[derive(Default)]
struct EdgeFold {
    in_degree: BTreeMap<String, usize>,
    intra_degree: BTreeMap<String, usize>,
    dep_matrix: BTreeMap<(String, String), usize>,
    referenced: BTreeSet<String>,
    import_edges: usize,
    signature_exposed: BTreeSet<String>,
    import_targets: BTreeSet<String>,
    reexporters: BTreeMap<String, Vec<String>>,
    foreign_reach: BTreeMap<String, ForeignReach>,
}

impl EdgeFold {
    /// Move the removal-sensitive subset out of the fold — the one
    /// [`DegreeMaps`] construction site, shared by the build pass and
    /// [`Assembly::refold_excluding`].
    fn take_degrees(&mut self) -> DegreeMaps {
        DegreeMaps {
            in_degree: std::mem::take(&mut self.in_degree),
            intra_degree: std::mem::take(&mut self.intra_degree),
            signature_exposed: std::mem::take(&mut self.signature_exposed),
            foreign_reach: std::mem::take(&mut self.foreign_reach),
        }
    }
}

impl Assembly {
    /// The crates whose pub API this config is entitled to *judge*: those
    /// with a lib / bin / proc-macro unit here (a lib's `+test` harness keeps
    /// its lib kind). Integration-test / bench target crates contribute
    /// usage only — their pubs are harness plumbing, not API surface.
    pub(super) fn candidate_crates(&self) -> BTreeSet<String> {
        const USAGE_ONLY: &[&str] = &["build", "test", "bench", "example"];
        self.fragments
            .iter()
            .map(|f| f.archived())
            .filter(|f| !USAGE_ONLY.contains(&f.target_kind.as_str()))
            .map(|f| f.crate_name.as_str().to_owned())
            .collect()
    }

    pub(super) fn build(fragments: Vec<FragmentBytes>, ids: Arc<IdentityIndex>) -> Self {
        // Build-script fragments are references-only carriers (`items` empty,
        // crate name always `build_script_build`) — not crates of the
        // assembly. Letting one in would insert a phantom member.
        let crates: BTreeSet<String> = fragments
            .iter()
            .map(|f| f.archived())
            .filter(|f| f.target_kind.as_str() != "build")
            .map(|f| f.crate_name.as_str().to_owned())
            .collect();
        // 1) Global def index (keyed by the cross-crate-stable DefPathHash) +
        //    the trait→impls linkage + the module-visibility table (the
        //    pub-module-hop substrate) + `id_key` (identity → this config's
        //    key, the landing map for a cross-config global-join hit).
        let mut defs = DefMap::default();
        let mut impls_of: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut module_vis = BTreeMap::new();
        let mut id_key: FastMap<String, String> = FastMap::default();
        let mut trait_members: Vec<(String, String)> = Vec::new();
        for fb in &fragments {
            let frag = fb.archived();
            for it in frag.items.iter() {
                let category = Category::of(it.parent_kind.as_deref(), it.trait_item.as_deref());
                if let Some(ti) = it.trait_item.as_deref() {
                    impls_of
                        .entry(ti.to_owned())
                        .or_default()
                        .push(it.key.as_str().to_owned());
                }
                if it.parent_kind.as_deref() == Some("trait") && it.path.len() > 1 {
                    trait_members.push((
                        it.key.as_str().to_owned(),
                        wl_ir::join_paths(&it.path[..it.path.len() - 1], "::"),
                    ));
                }
                if it.kind.as_str() == "mod" {
                    module_vis.insert(
                        wl_ir::join_paths(&it.path, "::"),
                        it.visibility == Visibility::Public,
                    );
                }
                id_key
                    .entry(wl_ir::join_paths(&it.path, "::"))
                    .or_insert_with(|| it.key.as_str().to_owned());
                defs.insert(
                    it.key.as_str().to_owned(),
                    DefInfo {
                        krate: frag.crate_name.as_str().to_owned(),
                        path: wl_ir::join_paths(&it.path, "::"),
                        kind: it.kind.as_str().to_owned(),
                        public: it.visibility == Visibility::Public,
                        category,
                        trait_item: it.trait_item.as_deref().map(str::to_owned),
                        synthetic: it.span.is_none(),
                        export_root: !it.attrs.is_empty(),
                        self_type: it.self_type.as_deref().map(str::to_owned),
                        span: it.span.as_ref().map(wl_ir::Span::from),
                        full_span: it.full_span.as_ref().map(wl_ir::Span::from),
                        vis_span: it.vis_span.as_ref().map(wl_ir::Span::from),
                        self_kind: it.self_kind.as_deref().map(str::to_owned),
                        self_copy: it.self_copy.as_ref().copied(),
                    },
                );
            }
        }

        // Resolve trait members to their owning trait declaration's key (the
        // parent path's identity — trait and member always land in the same
        // fragment, and `id_key` is already first-write-deterministic).
        let trait_parent: FastMap<String, String> = trait_members
            .into_iter()
            .filter_map(|(member, parent_path)| {
                id_key.get(&parent_path).map(|k| (member, k.clone()))
            })
            .collect();

        // 2) Path→key fallback index for BUILD-fragment edges: a build script's
        // dependencies compile in Build mode, whose `-Cmetadata` (hence
        // `StableCrateId`, hence `DefPathHash` generation) differs from the
        // Check-mode units the defs above were extracted from — a build
        // edge's `to_key` never joins `defs` directly, nor the global index
        // (Build-mode units are never extracted). Their `to` display path is
        // rooted at the defining crate, so path equality is the join; indexed
        // over lib-shaped fragments only (build deps can only be lib/proc-macro
        // targets — excluding bin/test fragments avoids same-crate-name path
        // collisions). Residual miss: a target rendered at a re-export path
        // (the visible-parent behavior) doesn't join and degrades to the
        // pre-build-fragment posture — the use goes unseen; never a false join.
        let mut path_key: FastMap<String, String> = FastMap::default();
        let mut name_key: FastMap<(String, String), Vec<(String, String)>> = FastMap::default();
        for fb in &fragments {
            let frag = fb.archived();
            if !matches!(frag.target_kind.as_str(), "lib" | "proc-macro") {
                continue;
            }
            for it in frag.items.iter() {
                path_key.insert(
                    wl_ir::join_paths(&it.path, "::"),
                    it.key.as_str().to_owned(),
                );
                if let (Some(first), Some(last)) = (it.path.first(), it.path.last()) {
                    name_key
                        .entry((first.as_str().to_owned(), last.as_str().to_owned()))
                        .or_default()
                        .push((
                            wl_ir::join_paths(&it.path, "::"),
                            it.key.as_str().to_owned(),
                        ));
                }
            }
        }

        // 3) Reverse index: fold every fragment's forward edges onto the def
        //    each resolves to. `fold_edges` reads `defs`/`ids`/`id_key`/
        //    `path_key`, so build the assembly with those in place and the
        //    edge-derived maps empty, then fold into them.
        let assoc_members = super::clippy_guard::assoc_members_index(&defs, &trait_parent);
        let fields_of = super::clippy_guard::fields_of_index(&defs, &id_key);
        let mut asm = Assembly {
            fragments,
            crates,
            defs,
            degrees: DegreeMaps::default(),
            dep_matrix: BTreeMap::new(),
            referenced: BTreeSet::new(),
            import_edges: 0,
            impls_of,
            assoc_members,
            fields_of,
            trait_parent,
            import_targets: BTreeSet::new(),
            reexporters: BTreeMap::new(),
            module_vis,
            path_key,
            name_key,
            ids,
            id_key,
        };
        let mut fold = asm.fold_edges(None);

        // A glob re-export (`pub use m::*`) resolves to the MODULE def, but
        // it re-exports every public def directly under it — expand so the
        // re-export guard and reachability see through globs, as syn's
        // re-export index did. (`pub use m;` is indistinguishable from
        // `pub use m::*` here; expanding both over-approximates toward
        // suppression — the safe direction. Plain `use` never lands in
        // `reexporters`, so a test-mod `use super::*` expands nothing.)
        //
        // Index the direct PUBLIC children of every module by their parent
        // path ONCE (O(defs)), so the expansion is O(re-exporting modules +
        // their children) rather than a full `defs` scan per re-exporting
        // module (O(modules × defs) — the dominant glob-expand cost at scale).
        // A def is a direct member of `parent` iff its path is `parent::<name>`
        // with no deeper `::`, i.e. `parent` is its path minus the trailing
        // segment — so `rsplit_once("::")` yields exactly that parent. Root
        // items (no `::`) have no parent module and match no glob, as before.
        let mut children_by_parent: std::collections::HashMap<&str, Vec<&str>> =
            std::collections::HashMap::new();
        for (child_key, child) in &asm.defs {
            if !child.public {
                continue;
            }
            if let Some((parent, _)) = child.path.rsplit_once("::") {
                children_by_parent
                    .entry(parent)
                    .or_default()
                    .push(child_key.as_str());
            }
        }
        let module_imports: Vec<(String, Vec<String>)> = fold
            .reexporters
            .iter()
            .filter(|(key, _)| asm.defs.get(*key).is_some_and(|d| d.kind == "mod"))
            .map(|(key, importers)| (asm.defs[key].path.clone(), importers.clone()))
            .collect();
        for (mod_path, importers) in module_imports {
            let Some(children) = children_by_parent.get(mod_path.as_str()) else {
                continue;
            };
            // Each child key is distinct and has a single parent, so the visit
            // order never affects a `reexporters` Vec (each is extended once)
            // nor the `import_targets` set — the expansion stays deterministic.
            for &child_key in children {
                fold.import_targets.insert(child_key.to_owned());
                fold.reexporters
                    .entry(child_key.to_owned())
                    .or_default()
                    .extend(importers.iter().cloned());
            }
        }

        asm.degrees = fold.take_degrees();
        asm.dep_matrix = fold.dep_matrix;
        asm.referenced = fold.referenced;
        asm.import_edges = fold.import_edges;
        asm.import_targets = fold.import_targets;
        asm.reexporters = fold.reexporters;
        asm
    }

    /// The def key an edge lands on, resolved through the whole join: the local
    /// exact hash join first (the fast path — target extracted in this config);
    /// then the **global** hash join, translated to this config's own key for
    /// the same identity (the target was extracted only under another config —
    /// a `+test`/bench/integration edge to a dependency's plain rlib carries
    /// that plain generation, and cargo freshness leaves it only in the primary
    /// dir); then the display-path fallback. `None` when the target resolves
    /// nowhere (std/third-party, or a ctor/variant we don't emit).
    ///
    /// The path fallback is last-resort for EVERY edge, not just build
    /// fragments: feature unification can give a member's plain rlib under
    /// `--tests` a `-Cmetadata` (hence `DefPathHash` generation) that NO
    /// config extracts — e.g. a `[features]`-gated integration test whose
    /// harness links the feature-unified lib (the LeaveDates
    /// `ChuckNorrisJokeEndpoint::new` E0624). Same soundness argument as the
    /// build-fragment join: `path_key` holds definition-rooted paths of
    /// lib-shaped fragments, so a hit is the def itself; a target rendered at
    /// a re-export path just misses and degrades to the identity fold —
    /// toward exemption, never a false lead.
    pub(super) fn resolve_key(&self, e: &ArchivedRefEdge) -> Option<&str> {
        if let Some((k, _)) = self.defs.get_key_value(e.to_key.as_str()) {
            return Some(k.as_str());
        }
        if let Some(id) = self.ids.identity_of(e.to_key.as_str())
            && let Some(k) = self.id_key.get(id)
        {
            return Some(k.as_str());
        }
        if let Some(k) = self.path_key.get(wl_ir::join_paths(&e.to, "::").as_str()) {
            return Some(k.as_str());
        }
        // Suffix-relaxed leg: the unextracted-generation target may render at
        // a RE-EXPORT path (`data_common::Fraction::new`,
        // `data_model::wallchart::WorkSchedule::WEEKDAYS…`) that exact
        // equality can't join to the definition path — re-exports both drop
        // AND keep intermediate modules, so only the trailing `Type::item`
        // (or bare `item`) is stable. Match crate root + that tail; taken
        // only on an unambiguous single hit, and crediting reach is the
        // exemption-safe direction.
        if e.to.len() >= 2
            && let (Some(first), Some(last)) = (e.to.first(), e.to.last())
            && let Some(cands) = self
                .name_key
                .get(&(first.as_str().to_owned(), last.as_str().to_owned()))
        {
            let last = last.as_str();
            let tail = match e.to.len() {
                2 => format!("::{last}"),
                _ => format!("::{}::{last}", e.to[e.to.len() - 2].as_str()),
            };
            let mut hits = cands.iter().filter(|(path, _)| path.ends_with(&tail));
            if let (Some((_, k)), None) = (hits.next(), hits.next()) {
                return Some(k.as_str());
            }
        }
        None
    }

    /// One pass over every fragment's forward edges, folded onto the def each
    /// [`resolve_key`](Self::resolve_key)s to — the reverse index the
    /// reachability verdicts read. An edge whose target no config extracted is
    /// ignored.
    ///
    /// `removed` (the unused-pub `--fix` cascade) drops the outgoing edges of
    /// any def it segment-covers, so a callee that def solely reached falls to
    /// zero in-degree; the build pass passes `None`. Two edge policies:
    /// `in_degree`/`intra_degree` count only real use-sites (`!import`) so a
    /// `pub use` can't mask a dead name; `dep_matrix`/`referenced` count every
    /// cross-crate edge (importing B's item is a use of B).
    fn fold_edges(&self, removed: Option<&RemovalSet>) -> EdgeFold {
        let mut f = EdgeFold::default();
        for fb in &self.fragments {
            let frag = fb.archived();
            for e in frag.references.iter() {
                // A trait-scope fact is import-resolution evidence, not a
                // use-site: the method call it certifies has its own edge,
                // so counting it here would double the trait's in-degree.
                if e.trait_scope {
                    continue;
                }
                // The removed item's own use-sites disappear with it.
                if removed.is_some_and(|r| r.covers(&e.from)) {
                    continue;
                }
                let Some(key) = self.resolve_key(e) else {
                    // No landing def anywhere local — but if the GLOBAL index
                    // knows the target, this config never extracted its crate
                    // (harness flag off) and the reach is recorded at identity
                    // level. A genuine out-of-workspace target isn't in the
                    // index and stays ignored.
                    self.fold_foreign(e, &mut f);
                    continue;
                };
                let def = &self.defs[key];
                // Signature exposure: the target is named in a PUB item's
                // signature (`from_key` is this fragment's own def — always a
                // local key; an unknown `from` can't be a pub API surface).
                // A trait member's visibility is inherited, so a bound like
                // `trait DateFn { type Date: ChronoExt; }` exposes ChronoExt
                // iff the OWNING TRAIT is pub (`private_bounds` otherwise).
                let from_public = self.defs.get(e.from_key.as_str()).is_some_and(|d| d.public)
                    || self
                        .trait_parent
                        .get(e.from_key.as_str())
                        .and_then(|tk| self.defs.get(tk))
                        .is_some_and(|t| t.public);
                if e.in_signature && from_public {
                    f.signature_exposed.insert(key.to_string());
                }
                // Package-granularity dependency tracking keeps the crate-NAME
                // comparison: a bin's use of its own package's lib is not a
                // dependency edge.
                let from_crate = e.from.first().map(|s| s.as_str()).unwrap_or_default();
                if from_crate != def.krate {
                    *f.dep_matrix
                        .entry((from_crate.to_string(), def.krate.clone()))
                        .or_insert(0) += 1;
                    f.referenced.insert(def.krate.clone());
                }
                if e.import {
                    f.import_edges += 1;
                    // Only a `pub use` (re-export) pins its target `pub`
                    // (E0364/E0365) or exposes it through the importing module.
                    if e.reexport {
                        f.import_targets.insert(key.to_string());
                        f.reexporters
                            .entry(key.to_string())
                            .or_default()
                            .push(wl_ir::join_paths(&e.from, "::"));
                    }
                    continue; // import: not a use-site for unused-pub
                }
                // The VISIBILITY split trusts the extractor's `CrateNum`
                // comparison, not the name: a sibling target of the same
                // package (bin `main.rs`, integration test) shares the crate
                // name but compiles as its OWN crate — `pub(crate)` cannot
                // reach it, so its edges must count as cross-crate.
                let cross = e.external;
                *f.in_degree.entry(key.to_string()).or_insert(0) += 1;
                if !cross {
                    *f.intra_degree.entry(key.to_string()).or_insert(0) += 1;
                }
                // A method-call edge lands on a trait *member* — the decl
                // (`parent_kind == "trait"`) or an impl's method (whose
                // `trait_item` names the decl) — never on the trait itself.
                // The owning trait must survive wherever its members are
                // used, so the member's reach folds onto the trait
                // declaration too.
                let member = def.trait_item.as_deref().unwrap_or(key);
                if let Some(trait_key) = self.trait_parent.get(member) {
                    *f.in_degree.entry(trait_key.clone()).or_insert(0) += 1;
                    if !cross {
                        *f.intra_degree.entry(trait_key.clone()).or_insert(0) += 1;
                    }
                }
            }
        }
        f
    }

    /// Credit an edge whose target the global index knows but this config has
    /// no def for (see [`ForeignReach`]). Mirrors the fold's own policies:
    /// signature exposure gated on a pub `from`, imports discounted from the
    /// use-site reach, cross/intra split by the extractor's `CrateNum`
    /// comparison ([`RefEdge::external`] — a same-package sibling target is a
    /// different crate despite the shared name). Trait-member reach is not
    /// folded onto the parent trait here: an identity-only target has no
    /// `parent_kind`, and the config that extracted the trait credits it.
    fn fold_foreign(&self, e: &ArchivedRefEdge, f: &mut EdgeFold) {
        let Some(id) = self.ids.identity_of(e.to_key.as_str()) else {
            return;
        };
        let fr = f.foreign_reach.entry(id.to_string()).or_default();
        if e.in_signature && self.defs.get(e.from_key.as_str()).is_some_and(|d| d.public) {
            fr.signature_exposed = true;
        }
        if e.import {
            return; // import: not a use-site for unused-pub
        }
        if e.external {
            fr.cross = true;
        } else {
            fr.intra = true;
        }
    }

    /// Iterate the archived fragments — the re-scan substrate the dangling-import
    /// and unused-deps queries walk. Each `archived()` is an O(1) cast.
    pub(super) fn archived_fragments(&self) -> impl Iterator<Item = &ArchivedIrFragment> {
        self.fragments.iter().map(|f| f.archived())
    }

    /// Borrow the prebuilt removal-sensitive indexes — the no-removal degree
    /// source for [`super::pub_usage::compute`].
    pub(super) fn degree_view(&self) -> DegreeView<'_> {
        self.degrees.view()
    }

    /// Recompute the removal-sensitive indexes (`in_degree`, `intra_degree`,
    /// `signature_exposed`) as if every def matched by `removed` had been
    /// deleted: its **outgoing** edges vanish, so a callee it solely reached
    /// drops to zero in-degree. Everything else the verdict reads
    /// (`module_vis`, `reexporters`, `import_targets`, `impls_of`, `defs`) is
    /// invariant under item deletion — a deletion candidate is, by
    /// construction, never a module, a `pub use` target, or referenced by a
    /// surviving item — so those are reused unchanged. The one-pass cascade
    /// substrate; see [`super::SemanticModel::pub_candidates_excluding`].
    ///
    /// Shares [`Assembly::fold_edges`] with the build pass (so the cross-config
    /// global join applies to the cascade too); the fold's deletion-invariant
    /// outputs (`dep_matrix`, import maps, …) are recomputed and discarded here.
    pub(super) fn refold_excluding(&self, removed: &RemovalSet) -> DegreeMaps {
        self.fold_edges(Some(removed)).take_degrees()
    }

    /// Resolve an edge's target to the def it lands on — the full join
    /// ([`Assembly::resolve_key`], display-path fallback included).
    /// `None` when the target is outside the workspace or not a tree item. The
    /// import-index substrate ([`super::SemanticModel::dangling_imports`]).
    pub(super) fn def_for_edge(&self, e: &ArchivedRefEdge) -> Option<&DefInfo> {
        self.resolve_key(e).and_then(|k| self.defs.get(k))
    }

    /// This config's stable key for a cross-config identity (a `::`-joined
    /// display path), if the config defines it.
    pub(super) fn key_for_identity(&self, id: &str) -> Option<&str> {
        self.id_key.get(id).map(String::as_str)
    }

    /// Is `id` (a `::`-joined display path) a module in this config? The
    /// scope-boundary test of the dangling-import check: a `use` statement's
    /// reach ends at its enclosing module, so the reference scope-chain walk
    /// stops at the nearest prefix this returns `true` for.
    pub(super) fn is_module(&self, id: &str) -> bool {
        self.id_key
            .get(id)
            .and_then(|k| self.defs.get(k))
            .is_some_and(|d| d.kind == "mod")
    }

    /// The cross-config identity an edge's target resolves to: through the def
    /// join when a landing def exists, else the global index alone (the
    /// foreign case — see [`ForeignReach`]). Lets the import surgery see a
    /// `use` of a removed item even from a config that never extracted the
    /// defining crate.
    pub(super) fn target_identity(&self, e: &ArchivedRefEdge) -> Option<&str> {
        match self.def_for_edge(e) {
            Some(def) => Some(def.path.as_str()),
            None => self.ids.identity_of(e.to_key.as_str()),
        }
    }

    /// Is `key`'s def named in some **pub** item's signature (an
    /// `in_signature` edge whose `from` is public)? Tightening such a def
    /// breaks compilation (E0446 / `private_interfaces`), so the unused-pub
    /// `--fix` must not propose it.
    pub fn exposed_in_public_signature(&self, key: &str) -> bool {
        self.degrees.signature_exposed.contains(key)
    }

    /// Is `key`'s def the target of a `use`/`pub use` declaration? Tightening
    /// or deleting it can break the re-export (E0364/E0365) — the unused-pub
    /// re-export guard.
    pub fn is_import_target(&self, key: &str) -> bool {
        self.import_targets.contains(key)
    }

    /// Every reference out of `krate`'s **primary-unit** fragments (lib, bin,
    /// proc-macro — never `"test"` or `"build"`: integration tests and build
    /// scripts legitimately reach across layers), with canonical targets and
    /// module attribution — the `architecture` lint's substrate.
    ///
    /// Lowered-signature edges are skipped: every surface-visible signature
    /// type also flows through the HIR walk (which carries the use-site span
    /// this query exists for); the lowered twin would only duplicate it at
    /// alias granularity, spanless.
    pub fn references_from(&self, krate: &str) -> Vec<ResolvedRef> {
        let mut out = Vec::new();
        for fb in &self.fragments {
            let frag = fb.archived();
            if frag.crate_name.as_str() != krate
                || matches!(frag.target_kind.as_str(), "test" | "build")
            {
                continue;
            }
            for e in frag.references.iter() {
                if e.in_signature || e.trait_scope {
                    continue;
                }
                // Canonical target: the *definition* path when the target is
                // a workspace def — rustc renders a consumer's view at the
                // re-export path (visible-parent map), so this join is what
                // sees through `pub use` chains, exactly like syn's
                // `resolve_canonical`. Out-of-workspace targets keep their
                // display path (rules may deny third-party crates by name).
                let to_path = match self.defs.get(e.to_key.as_str()) {
                    Some(def) => def.path.split("::").map(str::to_string).collect(),
                    None => e.to.iter().map(|s| s.as_str().to_owned()).collect(),
                };
                out.push(ResolvedRef {
                    module: self.enclosing_module(&e.from),
                    to_path,
                    to_kind: e.to_kind.as_str().to_owned(),
                    import: e.import,
                    glob: e.glob,
                    alias: e.alias.as_deref().map(str::to_owned),
                    span: e.span.as_ref().map(wl_ir::Span::from),
                });
            }
        }
        out
    }

    /// The nearest enclosing *module* of a `RefEdge::from` item path: the
    /// longest prefix naming a known module. An import edge's `from` IS its
    /// module (a `use`'s parent item is the module), so the full path is
    /// tried first; a code edge's `from` is the item, whose trailing
    /// segments (item name, impl-block renderings) never match a `mod` fact.
    /// Falls back to the crate root.
    fn enclosing_module<S: AsRef<str>>(&self, from: &[S]) -> Vec<String> {
        for end in (2..=from.len()).rev() {
            if self
                .module_vis
                .contains_key(wl_ir::join_paths(&from[..end], "::").as_str())
            {
                return from[..end].iter().map(|s| s.as_ref().to_owned()).collect();
            }
        }
        from.first()
            .map(|s| s.as_ref().to_owned())
            .into_iter()
            .collect()
    }

    /// External reachability: could an out-of-workspace consumer name this
    /// def? Three legs, all derived from emitted facts:
    ///
    /// 1. **Pub-module-hop**: the def is `pub` and every ancestor module on
    ///    its definition path is `pub` too. Judged from the `mod` ItemFacts
    ///    (path → visibility); segments without a matching `mod` fact —
    ///    impl-block renderings, trait names — are transparent.
    /// 2. **Re-export**: a `use` of the def sits in a module that is itself
    ///    module-hop reachable (`pub use` at the crate root being the common
    ///    case). The IR can't tell `pub use` from `use`, so this
    ///    over-approximates toward exemption — the safe direction for a
    ///    delete-suggesting lint. Chains through modules that are only
    ///    re-export-reachable themselves are not followed (rare; also
    ///    FN-safe).
    /// 3. **Inherent-impl items** flow through their self type
    ///    (`ItemFact::self_type` — the emitted key link, exact even for impl
    ///    blocks in a different module than the type): `Type::method` is
    ///    nameable iff `Type` is.
    pub fn is_externally_reachable(&self, key: &str, def: &DefInfo) -> bool {
        if !def.public {
            return false;
        }
        if def.category == Category::InherentImpl {
            return def.self_type.as_deref().is_some_and(|k| {
                self.defs
                    .get(k)
                    .is_some_and(|type_def| self.is_externally_reachable(k, type_def))
            });
        }
        self.module_hop_reachable(&def.path)
            || self
                .reexporters
                .get(key)
                .is_some_and(|importers| importers.iter().any(|m| self.module_hop_reachable(m)))
    }

    /// Every ancestor module on `path` (proper prefixes past the crate root)
    /// is `pub`. The crate root itself is trivially reachable.
    fn module_hop_reachable(&self, path: &str) -> bool {
        let segments: Vec<&str> = path.split("::").collect();
        for end in 2..segments.len() {
            let prefix = segments[..end].join("::");
            if let Some(vis) = self.module_vis.get(prefix.as_str())
                && !vis
            {
                return false;
            }
        }
        // A module path names the module itself — its own visibility gates
        // the hop too (a def path's last segment is the def, not a module,
        // and module_vis simply has no entry for it).
        if let Some(vis) = self.module_vis.get(path)
            && !vis
        {
            return false;
        }
        true
    }

    /// Is `krate`'s pub API an **external reachability boundary** — could an
    /// out-of-workspace consumer use it? With metadata this is "publishable
    /// library"; without, the `referenced` dependency-leaf proxy (which
    /// mislabels a published *leaf* library — metadata wins when present).
    pub(super) fn external_boundary(&self, krate: &str, meta: Option<&WorkspaceMeta>) -> bool {
        match meta {
            Some(m) => m.is_published_lib(krate),
            None => self.referenced.contains(krate),
        }
    }

    /// Reduce this config's `DefPathHash`-keyed candidate defs to the
    /// cross-config **stable identity** `(crate, def_path_str)` — level-1→2 of
    /// the union (SPIKE §7). A single `--tests` compile emits *both* a crate's
    /// plain and `+test` cfg variants (different `DefPathHash`, same path);
    /// OR-ing reach over every key sharing the identity folds them.
    pub(super) fn candidate_identities(&self) -> BTreeMap<String, CandReach> {
        let mut out: BTreeMap<String, CandReach> = BTreeMap::new();
        for (key, def) in &self.defs {
            if !def.public
                || def.synthetic
                || !def.category.is_candidate()
                || !CANDIDATE_KINDS.contains(&def.kind.as_str())
            {
                continue;
            }
            let reached = !matches!(self.reach_of(key, def), Reach::Unreached);
            let entry = out.entry(def.path.clone()).or_insert(CandReach {
                reached: false,
                krate: def.krate.clone(),
                category: def.category,
                kind: def.kind.clone(),
            });
            entry.reached |= reached;
        }
        out
    }

    /// Workspace-wide count of intra-crate-only references to `key` — kept for
    /// diagnostics that explain what the cross-crate join changed.
    pub fn intra_degree(&self, key: &str) -> usize {
        self.degrees.intra_degree.get(key).copied().unwrap_or(0)
    }

    /// The impl-item keys implementing a trait item (dispatch linkage).
    pub fn impls_of(&self, trait_item_key: &str) -> &[String] {
        self.impls_of
            .get(trait_item_key)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// The workspace crates `krate` references under this config (all edge
    /// kinds — importing another crate's item is a real use of that crate).
    pub fn crates_referenced_by(&self, krate: &str) -> BTreeSet<&str> {
        self.dep_matrix
            .keys()
            .filter(|(from, _)| from == krate)
            .map(|(_, to)| to.as_str())
            .collect()
    }

    /// `use`/re-export edges discounted from the unused-pub reverse index —
    /// an assembly stat for diagnostics.
    pub fn import_edge_count(&self) -> usize {
        self.import_edges
    }
}
