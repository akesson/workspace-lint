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

use wl_ir::{IrFragment, RefEdge, Visibility};

use super::join::IdentityIndex;
use super::meta::WorkspaceMeta;

/// How a def relates to the unused-pub verdict — derived from `parent_kind` +
/// `trait_item` (both rustc-emitted, no text heuristic).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    /// Module-level (parent `mod`/root) — a real unused-pub candidate.
    ModuleLevel,
    /// Inherent-impl item (`impl Foo { .. }`, no trait) — independently-
    /// controllable pub API, judged by its direct-call edges. Also a real
    /// candidate (the syn model can't even see these — a pivot win).
    InherentImpl,
    /// Trait-impl item (`impl Tr for Foo`) — reachable via trait dispatch the
    /// ref graph doesn't edge, visibility forced by the trait. Judged by
    /// dispatch expansion, not direct edges.
    TraitImpl,
    /// Trait *declaration* item, or body-nested / fn-local — excluded.
    Other,
}

impl Category {
    /// Classify from the rustc-emitted `parent_kind` + `trait_item`.
    fn of(parent_kind: Option<&str>, trait_item: &Option<String>) -> Self {
        match parent_kind {
            Some("mod") | None => Category::ModuleLevel,
            Some("impl") if trait_item.is_some() => Category::TraitImpl,
            Some("impl") => Category::InherentImpl,
            _ => Category::Other, // trait decl, fn-local, const/static bodies
        }
    }

    /// Is this a def the unused-pub verdict judges? Module-level and
    /// inherent-impl by direct use-site, trait-impl by dispatch expansion.
    /// Only `Other` (trait *declaration* items, fn-local defs) stays out.
    pub(super) fn is_candidate(self) -> bool {
        matches!(
            self,
            Category::ModuleLevel | Category::InherentImpl | Category::TraitImpl
        )
    }
}

/// How a candidate def is reached under *one* config — the per-config
/// primitive the unused-pub verdict and the cfg-matrix union both reduce to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// A real (non-import) use-site references this def directly. (The only
    /// way module-level / inherent-impl items are reached.)
    Direct,
    /// A trait-impl item whose implemented trait is **external** (std/serde/
    /// …). External code dispatches it invisibly (`format!` → `Display::fmt`),
    /// so it can never be proven dead — a sound root, never a lead.
    ExternalDispatch,
    /// A trait-impl item whose implemented trait is **workspace-internal** and
    /// whose trait method is dispatched somewhere (via generic/`dyn`), which
    /// reaches every impl of it.
    InternalDispatch,
    /// Carries an export-shaped attribute (`#[no_mangle]`/`#[export_name]`/
    /// `#[used]`): exported to the linker, no Rust referrer will ever exist —
    /// a sound root, never a lead (the `ffi_no_mangle_export` false positive
    /// this variant retires).
    ExportRoot,
    /// A judged candidate with no reaching edge under this config — a lead.
    Unreached,
}

/// One resolved reference out of a crate's primary-unit code — what
/// [`Assembly::references_from`] returns and the `architecture` lint judges.
/// `to_path` is canonical: the target's *definition* path for workspace defs
/// (re-export chains resolved), the display path as-referenced otherwise.
#[derive(Debug, Clone)]
pub struct ResolvedRef {
    /// Enclosing module of the use-site (crate-rooted definition path).
    pub module: Vec<String>,
    /// Canonical target path (see type docs).
    pub to_path: Vec<String>,
    /// The target's `DefKind` in the shared vocabulary (best-effort).
    pub to_kind: String,
    /// A `use`/`pub use` declaration, vs a code reference.
    pub import: bool,
    /// A glob import (`use m::*`) — judged as a representative child.
    pub glob: bool,
    /// Local binding name of a single-name import (`use a::B as C` ⇒ `C`).
    pub alias: Option<String>,
    /// Use-site span (file, on-disk byte range, 1-based line); `None` only
    /// for dummy-span edge cases.
    pub span: Option<wl_ir::Span>,
}

/// A def as the reverse index sees it — the owned copy keyed lookups return.
#[derive(Debug)]
pub struct DefInfo {
    pub krate: String,
    /// Display path, `[crate, ..]` joined with `::` — also the cross-config
    /// stable identity the union joins on.
    pub path: String,
    pub kind: String,
    pub public: bool,
    pub category: Category,
    /// For a trait-impl item, the stable key of the trait item it implements —
    /// the dispatch handle. `None` for every non-trait-impl def.
    pub trait_item: Option<String>,
    /// No source span ⇒ a compiler-synthesized def (the `--test` harness's
    /// generated `fn main`, …). Never an unused-pub candidate.
    pub synthetic: bool,
    /// Carries an export-shaped attribute (`ItemFact::attrs` non-empty) — a
    /// linker-visible root; see [`Reach::ExportRoot`].
    pub export_root: bool,
    /// For an inherent-impl item, the stable key of the impl's nominal self
    /// type — the external-reachability handle (see [`wl_ir::ItemFact::self_type`]).
    pub self_type: Option<String>,
    /// The whole-definition span (on-disk byte offsets — see [`wl_ir::Span`]).
    /// `None` exactly when `synthetic`.
    pub span: Option<wl_ir::Span>,
    /// The whole-**item** span — attrs/doc through the body — for the
    /// unused-pub `--fix` deletion surface (see [`wl_ir::ItemFact::full_span`]).
    /// `None` when there is no editable surface (synthetic / macro-generated).
    pub full_span: Option<wl_ir::Span>,
    /// The visibility-token span — the `--fix` tighten write surface. `None`
    /// when there is no independently editable token (private, trait-forced,
    /// macro-generated); see [`wl_ir::ItemFact::vis_span`].
    pub vis_span: Option<wl_ir::Span>,
}

/// One candidate def as the cfg-matrix union sees it: reduced to its
/// cross-config stable identity, carrying whether it was reached in *this*
/// config plus the display fields the union verdict reports.
#[derive(Debug)]
pub(crate) struct CandReach {
    pub(crate) reached: bool,
    pub(crate) krate: String,
    pub(crate) category: Category,
    pub(crate) kind: String,
}

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
    /// (segment-wise)? `from` is an edge's enclosing-item path.
    fn covers(&self, from: &[String]) -> bool {
        self.segs
            .iter()
            .any(|r| from.len() >= r.len() && from[..r.len()] == r[..])
    }
}

/// The removal-sensitive indexes recomputed by [`Assembly::refold_excluding`] —
/// the degree source [`super::pub_usage::compute`] reads instead of the
/// prebuilt maps when a cascade removal set is in effect.
pub(super) struct RemovalOverlay {
    in_degree: BTreeMap<String, usize>,
    intra_degree: BTreeMap<String, usize>,
    signature_exposed: BTreeSet<String>,
}

impl RemovalOverlay {
    pub(super) fn view(&self) -> DegreeView<'_> {
        DegreeView {
            in_degree: &self.in_degree,
            intra_degree: &self.intra_degree,
            signature_exposed: &self.signature_exposed,
        }
    }
}

/// A borrowed view of the three removal-sensitive indexes — either the
/// prebuilt maps ([`Assembly::degree_view`]) or a recomputed
/// [`RemovalOverlay`]. The degree source [`super::pub_usage::compute`] reads,
/// so the same fold serves both the plain and the cascade paths.
pub(super) struct DegreeView<'a> {
    pub(super) in_degree: &'a BTreeMap<String, usize>,
    pub(super) intra_degree: &'a BTreeMap<String, usize>,
    pub(super) signature_exposed: &'a BTreeSet<String>,
}

/// The kinds the unused-pub verdict judges. Widened from the spike's
/// `fn|struct|enum|trait|type` narrowing (migration PR 4): const/static/macro/
/// union pub items are candidates too — the per-lint `kinds` config filters at
/// the lint layer, not here. Exactly the syn model's `ItemKind::is_definition`
/// set (`mod` deliberately absent — a container, not a named definition).
pub(super) const CANDIDATE_KINDS: &[&str] = &[
    "fn", "struct", "enum", "trait", "type", "const", "static", "macro", "union",
];

/// One config's assembled workspace: fragments plus the derived global
/// indexes — the driver-backed replacement for the resolver's global view.
pub struct Assembly {
    fragments: Vec<IrFragment>,
    /// Every fragment crate's code-form name.
    pub(super) crates: BTreeSet<String>,
    /// Stable key → the def it identifies. The global symbol table.
    pub(super) defs: BTreeMap<String, DefInfo>,
    /// Stable key → **workspace-wide** in-degree of real (non-import)
    /// use-sites. The reverse index.
    in_degree: BTreeMap<String, usize>,
    /// Stable key → intra-crate-only in-degree (kept for reporting deltas).
    intra_degree: BTreeMap<String, usize>,
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
    /// Keys named in some PUB item's signature (`in_signature` edges whose
    /// `from` def is public) — the `exposed_in_public_signature` substrate.
    signature_exposed: BTreeSet<String>,
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
    path_key: BTreeMap<String, String>,
    /// The global (all-configs) key → identity index, for resolving an edge
    /// whose target was extracted only under another config (see
    /// [`Assembly::resolve_key`]).
    ids: Arc<IdentityIndex>,
    /// Cross-config identity → **this** config's key for it — the landing half
    /// of the global join (mirrors `path_key`'s shape). A global-index hit is
    /// translated through here to a local def before its use-site is counted,
    /// so every degree stays keyed by a def in `defs`. First fragment wins;
    /// deterministic because `load_fragments` sorts.
    id_key: BTreeMap<String, String>,
}

/// The eight edge-derived indexes produced by one pass of
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
}

impl Assembly {
    pub(super) fn build(fragments: Vec<IrFragment>, ids: Arc<IdentityIndex>) -> Self {
        // Build-script fragments are references-only carriers (`items` empty,
        // crate name always `build_script_build`) — not crates of the
        // assembly. Letting one in would insert a phantom member.
        let crates: BTreeSet<String> = fragments
            .iter()
            .filter(|f| f.target_kind != "build")
            .map(|f| f.crate_name.clone())
            .collect();

        // 1) Global def index (keyed by the cross-crate-stable DefPathHash) +
        //    the trait→impls linkage + the module-visibility table (the
        //    pub-module-hop substrate) + `id_key` (identity → this config's
        //    key, the landing map for a cross-config global-join hit).
        let mut defs = BTreeMap::new();
        let mut impls_of: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut module_vis = BTreeMap::new();
        let mut id_key: BTreeMap<String, String> = BTreeMap::new();
        for frag in &fragments {
            for it in &frag.items {
                let category = Category::of(it.parent_kind.as_deref(), &it.trait_item);
                if let Some(ti) = &it.trait_item {
                    impls_of.entry(ti.clone()).or_default().push(it.key.clone());
                }
                if it.kind == "mod" {
                    module_vis.insert(it.path.join("::"), it.visibility == Visibility::Public);
                }
                id_key
                    .entry(it.path.join("::"))
                    .or_insert_with(|| it.key.clone());
                defs.insert(
                    it.key.clone(),
                    DefInfo {
                        krate: frag.crate_name.clone(),
                        path: it.path.join("::"),
                        kind: it.kind.clone(),
                        public: it.visibility == Visibility::Public,
                        category,
                        trait_item: it.trait_item.clone(),
                        synthetic: it.span.is_none(),
                        export_root: !it.attrs.is_empty(),
                        self_type: it.self_type.clone(),
                        span: it.span.clone(),
                        full_span: it.full_span.clone(),
                        vis_span: it.vis_span.clone(),
                    },
                );
            }
        }

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
        let mut path_key: BTreeMap<String, String> = BTreeMap::new();
        for frag in &fragments {
            if !matches!(frag.target_kind.as_str(), "lib" | "proc-macro") {
                continue;
            }
            for it in &frag.items {
                path_key.insert(it.path.join("::"), it.key.clone());
            }
        }

        // 3) Reverse index: fold every fragment's forward edges onto the def
        //    each resolves to. `fold_edges` reads `defs`/`ids`/`id_key`/
        //    `path_key`, so build the assembly with those in place and the
        //    edge-derived maps empty, then fold into them.
        let mut asm = Assembly {
            fragments,
            crates,
            defs,
            in_degree: BTreeMap::new(),
            intra_degree: BTreeMap::new(),
            dep_matrix: BTreeMap::new(),
            referenced: BTreeSet::new(),
            import_edges: 0,
            impls_of,
            signature_exposed: BTreeSet::new(),
            import_targets: BTreeSet::new(),
            reexporters: BTreeMap::new(),
            module_vis,
            path_key,
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
        let module_imports: Vec<(String, Vec<String>)> = fold
            .reexporters
            .iter()
            .filter(|(key, _)| asm.defs.get(*key).is_some_and(|d| d.kind == "mod"))
            .map(|(key, importers)| (asm.defs[key].path.clone(), importers.clone()))
            .collect();
        for (mod_path, importers) in module_imports {
            let prefix = format!("{mod_path}::");
            for (child_key, child) in &asm.defs {
                if child.public
                    && child
                        .path
                        .strip_prefix(prefix.as_str())
                        .is_some_and(|rest| !rest.contains("::"))
                {
                    fold.import_targets.insert(child_key.clone());
                    fold.reexporters
                        .entry(child_key.clone())
                        .or_default()
                        .extend(importers.iter().cloned());
                }
            }
        }

        asm.in_degree = fold.in_degree;
        asm.intra_degree = fold.intra_degree;
        asm.dep_matrix = fold.dep_matrix;
        asm.referenced = fold.referenced;
        asm.import_edges = fold.import_edges;
        asm.signature_exposed = fold.signature_exposed;
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
    /// dir); then, for build fragments (`path_fallback`), the display-path
    /// fallback. `None` when no config extracted the target (std/third-party,
    /// or a ctor/variant we don't emit).
    fn resolve_key(&self, e: &RefEdge, path_fallback: bool) -> Option<&str> {
        if let Some((k, _)) = self.defs.get_key_value(&e.to_key) {
            return Some(k.as_str());
        }
        if let Some(id) = self.ids.identity_of(&e.to_key)
            && let Some(k) = self.id_key.get(id)
        {
            return Some(k.as_str());
        }
        if path_fallback && let Some(k) = self.path_key.get(&e.to.join("::")) {
            return Some(k.as_str());
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
        for frag in &self.fragments {
            let build = frag.target_kind == "build";
            for e in &frag.references {
                // The removed item's own use-sites disappear with it.
                if removed.is_some_and(|r| r.covers(&e.from)) {
                    continue;
                }
                let Some(key) = self.resolve_key(e, build) else {
                    continue; // target outside the workspace, or not a tree item
                };
                let def = &self.defs[key];
                // Signature exposure: the target is named in a PUB item's
                // signature (`from_key` is this fragment's own def — always a
                // local key; an unknown `from` can't be a pub API surface).
                if e.in_signature && self.defs.get(&e.from_key).is_some_and(|d| d.public) {
                    f.signature_exposed.insert(key.to_string());
                }
                let from_crate = e.from.first().map(String::as_str).unwrap_or_default();
                let cross = from_crate != def.krate;
                if cross {
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
                            .push(e.from.join("::"));
                    }
                    continue; // import: not a use-site for unused-pub
                }
                *f.in_degree.entry(key.to_string()).or_insert(0) += 1;
                if !cross {
                    *f.intra_degree.entry(key.to_string()).or_insert(0) += 1;
                }
            }
        }
        f
    }

    pub(super) fn fragments(&self) -> &[IrFragment] {
        &self.fragments
    }

    /// Reachability of a candidate def under this single-config assembly.
    /// Direct use-site first; then, for a trait-impl item, dispatch expansion
    /// off its `trait_item`: an external trait is a root (invisible dispatch),
    /// an internal one is reached iff its method is dispatched anywhere.
    /// Export-shaped attributes root a def unconditionally (linker-visible,
    /// no Rust referrer possible). Module-level and inherent-impl items carry
    /// no `trait_item`, so they fall through to Direct-or-Unreached.
    pub fn reach_of(&self, key: &str, def: &DefInfo) -> Reach {
        self.reach_with(key, def, &self.in_degree)
    }

    /// [`Assembly::reach_of`] against an explicit in-degree map — the
    /// unused-pub `--fix` cascade passes a [`RemovalOverlay`]'s recomputed
    /// in-degree so a def whose only referrers were deleted this pass reads as
    /// `Unreached` (dispatch reachability follows too: a trait item loses its
    /// `InternalDispatch` when its last dispatcher is removed).
    ///
    /// The dispatch lookups stay **local** (`self.defs` / `in_degree`, not the
    /// global cross-config index): a global miss falls through to
    /// `ExternalDispatch` = reached, the sound (never-a-false-lead) direction,
    /// and trait-impl items are judged but never *flagged* by the lint anyway;
    /// the identity union already ORs in the primary config's dispatch verdict.
    pub(super) fn reach_with(
        &self,
        key: &str,
        def: &DefInfo,
        in_degree: &BTreeMap<String, usize>,
    ) -> Reach {
        if in_degree.contains_key(key) {
            return Reach::Direct;
        }
        if def.export_root {
            return Reach::ExportRoot;
        }
        if let Some(ti) = &def.trait_item {
            if !self.defs.contains_key(ti) {
                return Reach::ExternalDispatch; // trait defined outside the workspace
            }
            if in_degree.contains_key(ti) {
                return Reach::InternalDispatch; // internal trait method is dispatched
            }
        }
        Reach::Unreached
    }

    /// Borrow the prebuilt removal-sensitive indexes — the no-removal degree
    /// source for [`super::pub_usage::compute`].
    pub(super) fn degree_view(&self) -> DegreeView<'_> {
        DegreeView {
            in_degree: &self.in_degree,
            intra_degree: &self.intra_degree,
            signature_exposed: &self.signature_exposed,
        }
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
    pub(super) fn refold_excluding(&self, removed: &RemovalSet) -> RemovalOverlay {
        let f = self.fold_edges(Some(removed));
        RemovalOverlay {
            in_degree: f.in_degree,
            intra_degree: f.intra_degree,
            signature_exposed: f.signature_exposed,
        }
    }

    /// Resolve an edge's target to the def it lands on — the full join
    /// ([`Assembly::resolve_key`], build-fragment path fallback enabled).
    /// `None` when the target is outside the workspace or not a tree item. The
    /// import-index substrate ([`super::SemanticModel::dangling_imports`]).
    pub(super) fn def_for_edge(&self, e: &RefEdge) -> Option<&DefInfo> {
        self.resolve_key(e, true).and_then(|k| self.defs.get(k))
    }

    /// Is `key`'s def named in some **pub** item's signature (an
    /// `in_signature` edge whose `from` is public)? Tightening such a def
    /// breaks compilation (E0446 / `private_interfaces`), so the unused-pub
    /// `--fix` must not propose it.
    pub fn exposed_in_public_signature(&self, key: &str) -> bool {
        self.signature_exposed.contains(key)
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
        for frag in &self.fragments {
            if frag.crate_name != krate || matches!(frag.target_kind.as_str(), "test" | "build") {
                continue;
            }
            for e in &frag.references {
                if e.in_signature {
                    continue;
                }
                // Canonical target: the *definition* path when the target is
                // a workspace def — rustc renders a consumer's view at the
                // re-export path (visible-parent map), so this join is what
                // sees through `pub use` chains, exactly like syn's
                // `resolve_canonical`. Out-of-workspace targets keep their
                // display path (rules may deny third-party crates by name).
                let to_path = match self.defs.get(&e.to_key) {
                    Some(def) => def.path.split("::").map(str::to_string).collect(),
                    None => e.to.clone(),
                };
                out.push(ResolvedRef {
                    module: self.enclosing_module(&e.from),
                    to_path,
                    to_kind: e.to_kind.clone(),
                    import: e.import,
                    glob: e.glob,
                    alias: e.alias.clone(),
                    span: e.span.clone(),
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
    fn enclosing_module(&self, from: &[String]) -> Vec<String> {
        for end in (2..=from.len()).rev() {
            if self
                .module_vis
                .contains_key(from[..end].join("::").as_str())
            {
                return from[..end].to_vec();
            }
        }
        from.first().cloned().into_iter().collect()
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
        self.intra_degree.get(key).copied().unwrap_or(0)
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
