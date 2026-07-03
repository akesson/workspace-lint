//! The single-config assembly: the multi-crate join (SPIKE §4/§5).
//!
//! Joins every per-crate `IrFragment` into a workspace-global def index and a
//! **cross-crate reverse index**, both keyed by the stable `DefPathHash`
//! (`ItemFact::key` / `RefEdge::to_key`) — **not** the display path:
//! `def_path_str` renders a def at its definition path in the defining crate
//! but at its re-export path in a consumer, so a path-equality join scores
//! 0/215 cross-crate (see `wl_ir::RefEdge`). Lifted from the spike assembler.

use std::collections::{BTreeMap, BTreeSet};

use wl_ir::{IrFragment, Visibility};

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
    /// Keys that are the target of a `use`/`pub use` declaration. Tightening
    /// or deleting such a def can break the re-export (E0364/E0365), so the
    /// unused-pub port suppresses these — the driver-backed analog of the syn
    /// re-export-index `is_target` guard. The IR doesn't distinguish `use`
    /// from `pub use`, so this over-approximates toward suppression (false
    /// negatives, never false positives).
    import_targets: BTreeSet<String>,
    /// Import-target key → the module paths whose `use` declarations name it
    /// (the edge's `from` — `visit_use` attributes a use-path to its enclosing
    /// module). The re-export leg of external reachability: a def `use`d in an
    /// externally-reachable module is nameable from outside through it.
    import_froms: BTreeMap<String, Vec<String>>,
    /// `mod` def path (joined) → is-public, for the pub-module-hop
    /// reachability judgement.
    module_vis: BTreeMap<String, bool>,
}

impl Assembly {
    pub fn build(fragments: Vec<IrFragment>) -> Self {
        let crates: BTreeSet<String> = fragments.iter().map(|f| f.crate_name.clone()).collect();

        // 1) Global def index (keyed by the cross-crate-stable DefPathHash) +
        //    the trait→impls linkage + the module-visibility table (the
        //    pub-module-hop substrate).
        let mut defs = BTreeMap::new();
        let mut impls_of: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut module_vis = BTreeMap::new();
        for frag in &fragments {
            for it in &frag.items {
                let category = Category::of(it.parent_kind.as_deref(), &it.trait_item);
                if let Some(ti) = &it.trait_item {
                    impls_of.entry(ti.clone()).or_default().push(it.key.clone());
                }
                if it.kind == "mod" {
                    module_vis.insert(it.path.join("::"), it.visibility == Visibility::Public);
                }
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
                        vis_span: it.vis_span.clone(),
                    },
                );
            }
        }

        // 2) Reverse index: union every fragment's forward edges onto the def
        //    `to_key` identifies. An edge whose target isn't a known def (std/
        //    third-party, or a ctor/variant we don't emit) is ignored.
        //
        //    Two edge policies: `in_degree`/`intra_degree` (unused-pub) count
        //    only real use-sites — `!import` — so a `pub use` doesn't mask a
        //    dead name; `dep_matrix`/`referenced` (unused-deps, leaf proxy)
        //    count all cross-crate edges.
        let mut in_degree = BTreeMap::new();
        let mut intra_degree = BTreeMap::new();
        let mut dep_matrix = BTreeMap::new();
        let mut referenced = BTreeSet::new();
        let mut import_edges = 0usize;
        let mut signature_exposed = BTreeSet::new();
        let mut import_targets = BTreeSet::new();
        let mut import_froms: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for frag in &fragments {
            for e in &frag.references {
                let Some(def) = defs.get(&e.to_key) else {
                    continue; // target outside the workspace, or not a tree item
                };
                // Signature exposure: the target is named in a PUB item's
                // signature (from-pub looked up via the def index; an unknown
                // `from` — e.g. a body-nested def we don't emit — can't be a
                // pub API surface, so it doesn't count).
                if e.in_signature && defs.get(&e.from_key).is_some_and(|f| f.public) {
                    signature_exposed.insert(e.to_key.clone());
                }
                let from_crate = e.from.first().cloned().unwrap_or_default();
                let cross = from_crate != def.krate;
                if cross {
                    *dep_matrix
                        .entry((from_crate, def.krate.clone()))
                        .or_insert(0) += 1;
                    referenced.insert(def.krate.clone());
                }
                if e.import {
                    import_edges += 1;
                    import_targets.insert(e.to_key.clone());
                    import_froms
                        .entry(e.to_key.clone())
                        .or_default()
                        .push(e.from.join("::"));
                    continue; // re-export/import: not a use-site for unused-pub
                }
                *in_degree.entry(e.to_key.clone()).or_insert(0) += 1;
                if !cross {
                    *intra_degree.entry(e.to_key.clone()).or_insert(0) += 1;
                }
            }
        }

        Assembly {
            fragments,
            crates,
            defs,
            in_degree,
            intra_degree,
            dep_matrix,
            referenced,
            import_edges,
            impls_of,
            signature_exposed,
            import_targets,
            import_froms,
            module_vis,
        }
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
        if self.in_degree.contains_key(key) {
            return Reach::Direct;
        }
        if def.export_root {
            return Reach::ExportRoot;
        }
        if let Some(ti) = &def.trait_item {
            if !self.defs.contains_key(ti) {
                return Reach::ExternalDispatch; // trait defined outside the workspace
            }
            if self.in_degree.contains_key(ti) {
                return Reach::InternalDispatch; // internal trait method is dispatched
            }
        }
        Reach::Unreached
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

    /// `(workspace-wide, intra-crate-only)` real-use in-degrees of `key`.
    /// A cross-crate use-site exists iff the first exceeds the second.
    pub(super) fn degrees(&self, key: &str) -> (usize, usize) {
        (
            self.in_degree.get(key).copied().unwrap_or(0),
            self.intra_degree.get(key).copied().unwrap_or(0),
        )
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
                .import_froms
                .get(key)
                .is_some_and(|froms| froms.iter().any(|m| self.module_hop_reachable(m)))
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
