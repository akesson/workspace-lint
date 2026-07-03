//! Phase-2 assembler — the multi-crate join (SPIKE-rustc-fidelity-tree.md §4/§5).
//!
//! Reads every per-crate `IrFragment` the driver emitted and assembles the
//! workspace-global view a single per-crate process *cannot* build (§5): a def
//! index and a **cross-crate reverse index**. Both are keyed by the stable
//! `DefPathHash` (`ItemFact::key` / `RefEdge::to_key`), **not** the display path
//! — `def_path_str` renders a def at its definition path in the defining crate
//! but at its re-export path in a consumer, so a path-equality join scores 0/215
//! cross-crate here (see `wl_ir::RefEdge`). The `DefPathHash` is identical no
//! matter which crate observed the def, so the reverse index unions every
//! crate's forward references onto the def that actually owns them.
//!
//! That union is what turns the per-crate unused-pub *candidate* count (steps
//! 2–3, intra-crate only, over-reporting) into a real workspace-wide verdict: a
//! pub item referenced from *no* workspace crate. The one honest caveat that
//! remains is external consumers of a *published* library's API — called out
//! per crate below.

use std::collections::{BTreeMap, BTreeSet};

use wl_ir::{IrFragment, Visibility};

fn main() {
    // Positional args are IR dirs; `--ws <root>` (optional) points at the target
    // workspace so we can read `cargo metadata` publish/target-kind roots (§7 step
    // 5). One dir ⇒ single-config report (step 4); many dirs ⇒ cfg-matrix union
    // (§7) — the first dir is the *primary* config defining the member-crate set.
    let mut dirs: Vec<String> = Vec::new();
    let mut ws_root: Option<String> = None;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        if let Some(v) = a.strip_prefix("--ws=") {
            ws_root = Some(v.to_string());
        } else if a == "--ws" {
            ws_root = it.next();
        } else {
            dirs.push(a);
        }
    }
    if dirs.is_empty() {
        dirs.push(std::env::var("WL_IR_OUT").unwrap_or_else(|_| "target/wl-ir".to_string()));
    }

    // Workspace metadata from cargo (publish + target kind + declared deps). `None`
    // ⇒ fall back to the dependency-leaf proxy for the boundary, with a note — the
    // proxy is right on this workspace but misclassifies a published *leaf* library
    // (see `Meta`) — and skip the `unused-deps` verdict (it needs declared deps).
    let meta = ws_root.as_deref().and_then(Meta::from_workspace);
    if ws_root.is_some() && meta.is_none() {
        eprintln!("wl-assemble: --ws given but `cargo metadata` failed; using leaf proxy");
    }

    let mut configs: Vec<(String, Assembly)> = Vec::new();
    for dir in &dirs {
        let fragments = load_fragments(dir);
        if fragments.is_empty() {
            eprintln!("wl-assemble: no IR fragments found in {dir}");
            std::process::exit(1);
        }
        configs.push((config_name(dir), Assembly::build(fragments)));
    }

    if configs.len() == 1 {
        let (_, asm) = &configs[0];
        asm.report(&dirs[0], meta.as_ref());
    } else {
        Matrix { configs: &configs }.report(meta.as_ref());
    }

    // The second lint riding the same IR (SPIKE §4 breadth): declared deps vs the
    // reference graph, unioned across the same configs. Needs `--ws` for the
    // manifest dep tables; without it, one line saying so.
    match meta.as_ref() {
        Some(m) => report_unused_deps(&configs, m),
        None => println!(
            "Unused-deps: skipped — pass `--ws <root>` for the cargo-metadata declared-dep tables."
        ),
    }
}

/// Which cargo dependency table a declared dep came from — the axis that decides
/// whether this IR can judge it. `Normal` deps compile in every lib/bin build;
/// `Dev` deps only when a test/example/bench target was compiled; `Build` deps
/// drive `build.rs`, which isn't lint-passed, so they're never judged.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DepKind {
    Normal,
    Dev,
    Build,
}

/// One declared dependency of a workspace member, as `cargo metadata` reports it —
/// the `unused-deps` unit of judgement.
struct DepDecl {
    /// The dependency crate's **package** name in code form (`-`→`_`). This is
    /// exactly what an edge target's crate segment (`RefEdge::to[0]`) carries — even
    /// under a `package = "…"` rename, because a crate's `tcx.crate_name` is its
    /// real name, not the local alias — so it joins directly against the exercised
    /// set with no rename bookkeeping.
    name: String,
    kind: DepKind,
    /// Feature-gated (`optional = true`): the crate isn't compiled unless its
    /// feature is on, so a config that didn't enable it can't observe usage. Never
    /// flagged here (skipped), to avoid calling a feature-gated dep dead.
    optional: bool,
}

/// Workspace facts from `cargo metadata --no-deps` (manifest parsing only — no
/// compile, toolchain-agnostic). Two lints read this one exec: the unused-pub
/// **boundary** (publish/target-kind roots, SPIKE §7 step 5) and the **declared
/// dependency tables** (the `unused-deps` substrate, SPIKE §4 breadth).
struct Meta {
    /// Every workspace member's code-form package name.
    members: BTreeSet<String>,
    /// Members that are publishable libraries — their pub API is an external
    /// reachability root (the unused-pub boundary). A pub item's visibility can only
    /// have out-of-workspace consumers if its crate is a publishable library; a
    /// bin's pub API and a `publish = false` lib's have no external root, so unused =
    /// dead. Principled replacement for the `referenced` dependency-leaf proxy: the
    /// proxy agrees on referenced crates but would tag a published **leaf** library
    /// (referenced by no workspace crate — like the marker crates here) as "dead".
    published_libs: BTreeSet<String>,
    /// Fragment crate-name (code form) → the member **package** that owns that
    /// target. A package has many targets (lib, bin, each integration test), and a
    /// dep declared once at the package level may be used by any of them — so we
    /// fold every target's edges back onto its owning package before judging.
    target_owner: BTreeMap<String, String>,
    /// Crate-names of test/example/bench targets. Dev-deps are judgeable only when
    /// one of these was actually compiled (a `--tests` config present); otherwise
    /// their usage is invisible and they must not be flagged.
    test_targets: BTreeSet<String>,
    /// Member package → its declared dependencies.
    declared: BTreeMap<String, Vec<DepDecl>>,
    /// Resolved dependency graph, code-name → direct dependency code-names (from
    /// `cargo metadata`'s `resolve`). The fix for **facade crates**: a declared dep
    /// like `clap` re-exports everything from `clap_builder`, so references resolve
    /// to the *defining* crate (`clap_builder`), never `clap`. Crediting a dep when
    /// any crate in its resolved closure is referenced clears that false positive.
    /// Empty if `resolve` is absent (degrades to exact-name matching).
    pkg_deps: BTreeMap<String, BTreeSet<String>>,
}

impl Meta {
    /// Read the target workspace via `cargo metadata` (WITH `resolve`, so the dep
    /// graph is available for facade-crate attribution). `None` if the command fails
    /// (missing cargo, bad path); the caller falls back to the leaf proxy for the
    /// boundary and skips unused-deps.
    fn from_workspace(root: &str) -> Option<Meta> {
        let md = cargo_metadata::MetadataCommand::new()
            .manifest_path(format!("{}/Cargo.toml", root.trim_end_matches('/')))
            .exec()
            .ok()?;

        // Members are the workspace packages only; `md.packages` also carries every
        // transitive dependency now that we resolve the graph.
        let member_ids: BTreeSet<String> = md
            .workspace_members
            .iter()
            .map(|id| id.to_string())
            .collect();
        // Package id → code-form crate name, over ALL packages (for the dep graph).
        let id_name: BTreeMap<String, String> = md
            .packages
            .iter()
            .map(|p| (p.id.to_string(), p.name.to_string().replace('-', "_")))
            .collect();
        // Resolved dep edges by code-name (all kinds — an over-approx closure only
        // ever over-credits, i.e. risks a false negative, never a false positive).
        let mut pkg_deps: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        if let Some(resolve) = &md.resolve {
            for node in &resolve.nodes {
                let Some(name) = id_name.get(&node.id.to_string()) else {
                    continue;
                };
                let entry = pkg_deps.entry(name.clone()).or_default();
                for dep_id in &node.dependencies {
                    if let Some(dn) = id_name.get(&dep_id.to_string()) {
                        entry.insert(dn.clone());
                    }
                }
            }
        }

        let mut members = BTreeSet::new();
        let mut published_libs = BTreeSet::new();
        let mut target_owner = BTreeMap::new();
        let mut test_targets = BTreeSet::new();
        let mut declared = BTreeMap::new();
        for p in &md.packages {
            if !member_ids.contains(&p.id.to_string()) {
                continue; // a transitive dependency, not a workspace member
            }
            let pkg = p.name.to_string().replace('-', "_");
            members.insert(pkg.clone());

            // `publish`: None ⇒ any registry; Some([]) ⇒ `publish = false`.
            let publishable = p.publish.as_ref().map(|v| !v.is_empty()).unwrap_or(true);
            let mut has_lib = false;
            for t in &p.targets {
                let tname = t.name.replace('-', "_");
                target_owner.insert(tname.clone(), pkg.clone());
                // Compare kinds via Display so we don't couple to cargo_metadata's
                // enum representation. A target may carry several kinds; classify by
                // any that matters (lib for the boundary, dev-target for dev-deps).
                for k in &t.kind {
                    match k.to_string().as_str() {
                        "lib" | "rlib" | "dylib" | "cdylib" | "staticlib" | "proc-macro" => {
                            has_lib = true;
                        }
                        "test" | "example" | "bench" => {
                            test_targets.insert(tname.clone());
                        }
                        _ => {} // bin, custom-build (build.rs — not lint-passed)
                    }
                }
            }
            if publishable && has_lib {
                published_libs.insert(pkg.clone());
            }

            let decls = p
                .dependencies
                .iter()
                .map(|d| DepDecl {
                    name: d.name.replace('-', "_"),
                    kind: match d.kind {
                        cargo_metadata::DependencyKind::Development => DepKind::Dev,
                        cargo_metadata::DependencyKind::Build => DepKind::Build,
                        _ => DepKind::Normal, // Normal + any future/unknown kind
                    },
                    optional: d.optional,
                })
                .collect();
            declared.insert(pkg, decls);
        }
        Some(Meta {
            members,
            published_libs,
            target_owner,
            test_targets,
            declared,
            pkg_deps,
        })
    }

    fn is_published_lib(&self, krate: &str) -> bool {
        self.published_libs.contains(krate)
    }

    /// The resolved dependency **closure** of a crate — the crate itself plus every
    /// crate reachable from it in the resolve graph. A declared dep is "exercised"
    /// if the referenced-crate set intersects this: a reference to `clap_builder`
    /// counts as using the declared facade `clap`, because `clap_builder ∈
    /// closure(clap)`. Over-approximate on purpose (shared transitive crates may
    /// credit more than one dep) — that only ever *misses* a truly-unused dep (false
    /// negative), never flags a used one (the safe direction for a "delete it" lint).
    fn dep_closure(&self, name: &str) -> BTreeSet<String> {
        let mut seen = BTreeSet::new();
        let mut stack = vec![name.to_string()];
        while let Some(n) = stack.pop() {
            if !seen.insert(n.clone()) {
                continue;
            }
            if let Some(deps) = self.pkg_deps.get(&n) {
                stack.extend(deps.iter().cloned());
            }
        }
        seen
    }
}

/// A short label for a config dir — its final path component (`…/matrix/tests`
/// → `tests`), falling back to the whole string.
fn config_name(dir: &str) -> String {
    dir.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(dir)
        .to_string()
}

fn load_fragments(dir: &str) -> Vec<IrFragment> {
    let mut fragments = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("wl-assemble: cannot read IR dir {dir}: {e}");
            eprintln!("hint: run the driver/embed first (see spike/README.md).");
            std::process::exit(1);
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        match std::fs::read_to_string(&path).map(|s| serde_json::from_str::<IrFragment>(&s)) {
            Ok(Ok(frag)) => {
                // Skew detection: a stale or foreign-build fragment must fail the
                // run, not silently assemble alongside current-schema fragments.
                if let Err(e) = frag.check_schema() {
                    eprintln!("wl-assemble: {}: {e}", path.display());
                    std::process::exit(1);
                }
                fragments.push(frag);
            }
            Ok(Err(e)) => eprintln!("wl-assemble: bad IR {}: {e}", path.display()),
            Err(e) => eprintln!("wl-assemble: read {}: {e}", path.display()),
        }
    }
    fragments.sort_by(|a, b| a.crate_name.cmp(&b.crate_name));
    fragments
}

/// How a def relates to the unused-pub verdict — derived from `parent_kind` +
/// `trait_item` (both rustc-emitted, no text heuristic).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Category {
    /// Module-level (parent `mod`/root) — a real unused-pub candidate.
    ModuleLevel,
    /// Inherent-impl item (`impl Foo { .. }`, no trait) — independently-
    /// controllable pub API, judged by its direct-call edges. **Also a real
    /// candidate** (this is what step 4a added; it used to be lumped with the
    /// trait impls and excluded wholesale).
    InherentImpl,
    /// Trait-impl item (`impl Tr for Foo`) — reachable via trait dispatch the
    /// ref graph doesn't edge, visibility forced by the trait. Excluded from the
    /// verdict (rustc's own dead_code excludes these too).
    TraitImpl,
    /// Trait *declaration* item, or body-nested / fn-local — excluded.
    Other,
}

/// A def as the reverse index needs to see it — cheap owned copy of the fields
/// keyed lookups return (the fragments stay around for per-crate reporting).
struct DefInfo {
    krate: String,
    path: String,
    kind: String,
    public: bool,
    category: Category,
    /// For a trait-impl item, the stable key of the trait item it implements
    /// (`ItemFact::trait_item`) — the dispatch handle: reachability of this impl
    /// item is decided by whether that trait method is dispatched (or the trait
    /// is external). `None` for every non-trait-impl def.
    trait_item: Option<String>,
    /// No source span (`ItemFact::span == None`) ⇒ a compiler-synthesized def —
    /// the `--test` harness's generated `fn main`, and other generated entry
    /// points. These are **never** unused-pub candidates (there's no source to
    /// remove, and they only appear in cfg-variant fragments); excluded so a
    /// `--tests` config's synthetic `main` doesn't masquerade as dead API.
    synthetic: bool,
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
    /// Is this a def the unused-pub verdict judges? All three "real" categories:
    /// module-level and inherent-impl by direct use-site, trait-impl by dispatch
    /// expansion (step 4a — trait-impls used to be excluded wholesale). Only
    /// `Other` (trait *declaration* items, fn-local defs) stays out of the verdict.
    fn is_candidate(self) -> bool {
        matches!(
            self,
            Category::ModuleLevel | Category::InherentImpl | Category::TraitImpl
        )
    }
}

/// How a candidate def is reached under *one* config — the per-config primitive
/// the unused-pub verdict and the cfg-matrix union both reduce to. Trait-dispatch
/// reachability (step 4a): a trait-impl item is judged by its *trait method*, not
/// its own edges, because it's invoked through dispatch the ref graph can't edge.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reach {
    /// A real (non-import) use-site references this def directly. (The only way
    /// module-level / inherent-impl items are reached.)
    Direct,
    /// A trait-impl item whose implemented trait is **external** (std/serde/clap/
    /// …). External code dispatches it invisibly (`format!` → `Display::fmt`,
    /// serde → `Deserialize::deserialize`), so it can never be proven dead — a
    /// sound root, never a lead. This is the "external-trait roots" the gap named.
    ExternalDispatch,
    /// A trait-impl item whose implemented trait is **workspace-internal** and
    /// whose trait method is dispatched somewhere (`Tr::f` via generic/`dyn`),
    /// which reaches every impl of it.
    InternalDispatch,
    /// A judged candidate with no reaching edge under this config — a lead.
    Unreached,
}

/// One candidate def as the cfg-matrix union sees it: reduced to its cross-config
/// stable identity (`DefInfo::path`), carrying whether it was reached in *this*
/// config plus the display fields the union verdict prints.
struct CandReach {
    /// Reached (any `Reach` variant but `Unreached`) under this config.
    reached: bool,
    krate: String,
    category: Category,
    kind: String,
}

/// The assembled workspace: fragments plus the derived global indexes. This is
/// the driver-backed replacement for `syn-workspace`'s `Workspace::load` — the
/// object the cross-crate lints will query.
struct Assembly {
    fragments: Vec<IrFragment>,
    /// Every workspace crate's code-form name.
    crates: BTreeSet<String>,
    /// Stable key → the def it identifies. The global symbol table.
    defs: BTreeMap<String, DefInfo>,
    /// Stable key → **workspace-wide** in-degree (intra + cross-crate). The
    /// reverse index: "how many distinct workspace items reference this def."
    in_degree: BTreeMap<String, usize>,
    /// Stable key → **intra-crate-only** in-degree — the old per-crate signal,
    /// kept to show what the cross-crate join buys.
    intra_degree: BTreeMap<String, usize>,
    /// (from-crate, defining-crate) → edge count, workspace-internal only. The
    /// raw `unused-deps` signal: a declared dep with a 0 here is unexercised.
    dep_matrix: BTreeMap<(String, String), usize>,
    /// Crates referenced by *some other* workspace crate. A crate absent here is
    /// a workspace leaf (nothing downstream in-workspace) — so its unused-pub is
    /// a real verdict, not merely "unused internally."
    referenced: BTreeSet<String>,
    /// Count of `use`/re-export edges discounted from the reverse index — the
    /// re-exports that would otherwise mask dead public names.
    import_edges: usize,
    /// Trait-item key → the impl-item keys that implement it. The trait→impls
    /// linkage (step 4a): when a trait method is used via dispatch, every impl
    /// of it is reachable. Built from each trait-impl item's `trait_item`.
    impls_of: BTreeMap<String, Vec<String>>,
}

impl Assembly {
    fn build(fragments: Vec<IrFragment>) -> Self {
        let crates: BTreeSet<String> = fragments.iter().map(|f| f.crate_name.clone()).collect();

        // 1) Global def index (keyed by the cross-crate-stable DefPathHash) +
        //    the trait→impls linkage.
        let mut defs = BTreeMap::new();
        let mut impls_of: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for frag in &fragments {
            for it in &frag.items {
                let category = Category::of(it.parent_kind.as_deref(), &it.trait_item);
                if let Some(ti) = &it.trait_item {
                    impls_of.entry(ti.clone()).or_default().push(it.key.clone());
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
                    },
                );
            }
        }

        // 2) Reverse index: union every fragment's forward edges onto the def
        //    `to_key` identifies. An edge whose target isn't a known def (std /
        //    third-party, or a ctor/variant/field we don't emit as an item) has
        //    no entry in `defs` and is correctly ignored for usage.
        //
        //    Two indexes with different edge policies:
        //    * `in_degree`/`intra_degree` (the unused-pub verdict) count only
        //      **real use-sites** — `!import` — so a `pub use` re-export doesn't
        //      mask a dead name.
        //    * `dep_matrix`/`referenced` (unused-deps, leaf detection) count
        //      **all** cross-crate edges — importing B's item is a real use of B.
        let mut in_degree = BTreeMap::new();
        let mut intra_degree = BTreeMap::new();
        let mut dep_matrix = BTreeMap::new();
        let mut referenced = BTreeSet::new();
        let mut import_edges = 0usize;
        for frag in &fragments {
            for e in &frag.references {
                let Some(def) = defs.get(&e.to_key) else {
                    continue; // target outside the workspace, or not a tree item
                };
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
        }
    }

    /// Reachability of a candidate def under this single-config assembly — the
    /// full trait-dispatch judgment. Direct use-site first; then, for a trait-impl
    /// item, dispatch expansion off its `trait_item`: an external trait is a root
    /// (invisible dispatch), an internal one is reached iff its method is
    /// dispatched anywhere (`in_degree` of the trait-method key > 0). Module-level
    /// and inherent-impl items carry no `trait_item`, so they fall straight through
    /// to `Direct`-or-`Unreached` — exactly the pre-4a judgment, now unified.
    fn reach_of(&self, key: &str, def: &DefInfo) -> Reach {
        if self.in_degree.contains_key(key) {
            return Reach::Direct;
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

    /// Is `krate`'s pub API an **external reachability boundary** — i.e. could an
    /// out-of-workspace consumer use it? With `Meta` (from `cargo metadata`) this
    /// is "publishable library"; without, it falls back to the `referenced`
    /// dependency-leaf proxy (SPIKE §7 step 5). An unused pub item in a boundary
    /// crate is *API surface* (a root, not dead); in a non-boundary crate (bin /
    /// `publish = false`) it is a hard **verdict** (nothing can reach it).
    fn external_boundary(&self, krate: &str, meta: Option<&Meta>) -> bool {
        match meta {
            Some(m) => m.is_published_lib(krate),
            None => self.referenced.contains(krate),
        }
    }

    /// Reduce this config's `DefPathHash`-keyed candidate defs to the cross-config
    /// **stable identity** `(crate, def_path_str)` — level-1→level-2 of the union
    /// (SPIKE §7). Within a config, `reach_of` already joined cross-crate on
    /// `DefPathHash`; here we collapse to the identity string (`DefInfo::path`,
    /// which is `[crate, ..def_path_str]` joined) and OR the reach over every key
    /// that shares it. A single `--tests` compile emits *both* a crate's plain and
    /// `+test` cfg variants (different `DefPathHash`, same path) — OR-ing over keys
    /// folds them, so an item used only by the `+test` variant reads reached.
    /// Verified 0 same-path candidate collisions within a cfg on this workspace, so
    /// the identity is safe (else §7's semantic-key fallback).
    fn candidate_identities(&self) -> BTreeMap<String, CandReach> {
        let mut out: BTreeMap<String, CandReach> = BTreeMap::new();
        for (key, def) in &self.defs {
            if !def.public
                || def.synthetic
                || !def.category.is_candidate()
                || !matches!(def.kind.as_str(), "fn" | "struct" | "enum" | "trait" | "type")
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

    fn report(&self, dir: &str, meta: Option<&Meta>) {
        let total_defs = self.defs.len();
        let total_edges: usize = self.fragments.iter().map(|f| f.references.len()).sum();
        println!(
            "Assembled {} crate fragment(s) from {dir}: {} defs, {} reference edges \
             ({} import/re-export edges discounted from the unused-pub index)\n",
            self.fragments.len(),
            total_defs,
            total_edges,
            self.import_edges,
        );
        self.report_per_crate();
        self.report_dep_matrix();
        self.report_unused_pub(meta);
    }

    /// One line per crate: item/pub counts and this crate's outbound edges.
    fn report_per_crate(&self) {
        println!("Crates:");
        for frag in &self.fragments {
            let pub_count = frag
                .items
                .iter()
                .filter(|i| i.visibility == Visibility::Public)
                .count();
            let (mut intra, mut cross, mut ext) = (0usize, 0usize, 0usize);
            for e in &frag.references {
                match (e.external, self.defs.contains_key(&e.to_key)) {
                    (false, _) => intra += 1,
                    (true, true) => cross += 1, // cross-crate but within the workspace
                    (true, false) => ext += 1,  // std / third-party
                }
            }
            println!(
                "  {:<22} {:>4} items ({:>3} pub)   edges: {intra} intra, {cross} workspace, {ext} external",
                frag.crate_name,
                frag.items.len(),
                pub_count,
            );
        }
        println!();
    }

    /// Cross-crate dependency matrix — the `unused-deps` substrate. Each entry
    /// is a workspace crate actually exercising another's API, by edge volume.
    fn report_dep_matrix(&self) {
        println!("Workspace dependency edges (from → defining crate):");
        if self.dep_matrix.is_empty() {
            println!("  (none — no crate references another workspace crate)");
        }
        for ((from, to), n) in &self.dep_matrix {
            println!("  {from} → {to}: {n} edges");
        }
        // A crate that references nothing but is referenced is a pure provider;
        // one referenced by no other workspace crate is a leaf (verdict below).
        let leaves: Vec<&String> = self
            .crates
            .iter()
            .filter(|c| !self.referenced.contains(*c))
            .collect();
        println!(
            "  workspace leaves (nothing downstream in-workspace): {}\n",
            leaves
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    /// The unused-pub payload. Every query runs off the assembled index (Phase 2
    /// on the global model). Candidates are **all three real categories** —
    /// module-level, inherent-impl, *and* trait-impl (step 4a: trait-impls are now
    /// judged by trait-dispatch reachability, not excluded wholesale). Each is
    /// classified by [`Assembly::reach_of`]: a direct use-site, external-trait
    /// dispatch (a sound root), internal-trait dispatch, or unreached (a lead).
    /// Reported: the verdict table, the leads, the cross-crate false positives the
    /// reverse index cleared, and the trait-dispatch reachability breakdown.
    fn report_unused_pub(&self, meta: Option<&Meta>) {
        #[derive(Default)]
        struct Row {
            cand: usize,   // judged candidates: mod-level + inherent + trait-impl
            leads: usize,  // unreached under this config
            ti_ext: usize, // trait-impl reached via external dispatch (immune root)
            ti_int: usize, // trait-impl reached via internal-trait dispatch
        }
        struct Lead {
            cat: Category,
            kind: String,
            path: String,
            boundary: bool, // crate's pub API is an external boundary (published lib)
            trait_path: Option<String>, // for a trait-impl lead: the trait method
        }
        let mut rows: BTreeMap<&str, Row> = BTreeMap::new();
        let mut leads: Vec<Lead> = Vec::new();
        let mut cleared: Vec<(String, String, usize)> = Vec::new(); // direct: intra=0, ws>0
        for (key, def) in &self.defs {
            if !def.public
                || def.synthetic
                || !def.category.is_candidate()
                || !matches!(def.kind.as_str(), "fn" | "struct" | "enum" | "trait" | "type")
            {
                continue;
            }
            let row = rows.entry(def.krate.as_str()).or_default();
            row.cand += 1;
            match self.reach_of(key, def) {
                Reach::Direct => {
                    // Unreferenced within its own crate but used by another — the
                    // canonical cross-crate false positive the reverse index
                    // prevents. Only meaningful for the direct-edge categories.
                    if def.category != Category::TraitImpl
                        && !self.intra_degree.contains_key(key)
                    {
                        cleared.push((def.kind.clone(), def.path.clone(), self.in_degree[key]));
                    }
                }
                Reach::ExternalDispatch => row.ti_ext += 1,
                Reach::InternalDispatch => row.ti_int += 1,
                Reach::Unreached => {
                    row.leads += 1;
                    leads.push(Lead {
                        cat: def.category,
                        kind: def.kind.clone(),
                        path: def.path.clone(),
                        boundary: self.external_boundary(&def.krate, meta),
                        trait_path: def
                            .trait_item
                            .as_ref()
                            .and_then(|ti| self.defs.get(ti))
                            .map(|d| d.path.clone()),
                    });
                }
            }
        }

        let boundary_src = if meta.is_some() {
            "publish/target-kind"
        } else {
            "dependency-leaf proxy"
        };
        println!("Unused-pub verdict (module-level + inherent-impl + trait-impl pub items):");
        println!("  crate boundary from: {boundary_src}");
        println!("  {:<22} {:>10}  {:>6}  {:>18}", "crate", "candidates", "leads", "trait-impl reach");
        let empty = Row::default();
        for krate in &self.crates {
            let row = rows.get(krate.as_str()).unwrap_or(&empty);
            let tag = if self.external_boundary(krate, meta) {
                "published lib → API surface"
            } else {
                "bin/internal → verdict"
            };
            println!(
                "  {:<22} {:>10}  {:>6}  ext {:>3} / int {:>3}   ({tag})",
                krate, row.cand, row.leads, row.ti_ext, row.ti_int,
            );
        }
        if leads.is_empty() {
            println!("  → no unreached pub candidates in the workspace.");
        } else {
            // Dead verdicts first (the actionable findings), then API surface.
            leads.sort_by(|a, b| a.boundary.cmp(&b.boundary).then(a.path.cmp(&b.path)));
            let dead = leads.iter().filter(|l| !l.boundary).count();
            println!(
                "  → unreached pub candidates ({}: {dead} dead verdict, {} API surface):",
                leads.len(),
                leads.len() - dead,
            );
            for lead in leads.iter().take(16) {
                let origin = match lead.cat {
                    Category::InherentImpl => "inherent",
                    Category::TraitImpl => "trait-impl",
                    _ => "mod-level",
                };
                let note = if lead.boundary {
                    "published API — external consumers possible"
                } else {
                    "verdict (dead)"
                };
                let via = lead
                    .trait_path
                    .as_ref()
                    .map(|t| format!("  impls internal {t} (never dispatched)"))
                    .unwrap_or_default();
                println!("    [{origin}] {:<6} {}  ({note}){via}", lead.kind, lead.path);
            }
        }
        println!();

        if !cleared.is_empty() {
            cleared.sort();
            println!(
                "Cross-crate false positives the reverse index cleared ({} — unused in own \
                 crate, used by another):",
                cleared.len()
            );
            for (kind, path, n) in cleared.iter().take(8) {
                println!("    {kind:<6} {path}  (ws refs: {n})");
            }
            println!();
        }

        let ext = rows.values().map(|r| r.ti_ext).sum::<usize>();
        let int = rows.values().map(|r| r.ti_int).sum::<usize>();
        let ti_leads = leads.iter().filter(|l| l.cat == Category::TraitImpl).count();
        self.report_trait_reachability(ext, int, ti_leads);
    }

    /// The trait-dispatch reachability breakdown (step 4a, now *judged* not
    /// excluded). Trait-impl items no longer sit outside the verdict: each is
    /// classified by [`Assembly::reach_of`]. External-trait impls (std/serde/clap
    /// — `Display::fmt`, `Deserialize::deserialize`) are sound roots: external code
    /// dispatches them invisibly, so they're immune, never leads. Internal-trait
    /// impls are reached iff their trait method is dispatched anywhere — otherwise
    /// they're genuine leads (the trait itself is dead). The `impls_of` linkage is
    /// the dispatch substrate this judgment reads.
    fn report_trait_reachability(&self, ext: usize, int: usize, ti_leads: usize) {
        let distinct = self.impls_of.len();
        let ws_internal: Vec<(&String, usize)> = self
            .impls_of
            .iter()
            .filter_map(|(ti, impls)| self.defs.get(ti).map(|d| (&d.path, impls.len())))
            .collect();
        println!("Trait-impl reachability (step 4a — judged via dispatch, not excluded):");
        println!("  {ext:>4} reached via external dispatch (std/serde/clap — sound roots, immune)");
        println!("  {int:>4} reached via internal-trait dispatch (their `Tr::f` is called)");
        println!("  {ti_leads:>4} unreached (internal trait never dispatched — genuine leads)");
        println!(
            "  trait→impls linkage: {distinct} distinct trait items implemented \
             ({} workspace-internal, {} external like std/serde)",
            ws_internal.len(),
            distinct - ws_internal.len(),
        );
        let mut top = ws_internal;
        top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        for (path, n) in top.iter().take(5) {
            println!("    {path} ← {n} impls");
        }
        if int == 0 && ti_leads == 0 {
            println!(
                "    (0 internal-dispatch / 0 leads: every workspace-internal trait here is \
                 pub(crate),\n     so its impls aren't *pub* candidates — the external-trait \
                 roots are what this\n     workspace exercises. The internal-dispatch branch \
                 fires for a *pub* internal trait.)"
            );
        }
        println!(
            "\n  Note: this is ONE config's verdict — a pub item used only under another cfg \
             (e.g.\n  `--test`) reads as a lead here. Pass multiple config dirs to `wl-assemble` \
             for the\n  cfg-matrix union that retires those (SPIKE §7)."
        );
    }
}

/// The cfg-matrix union (SPIKE §7) — Phase-2 over *several* single-config
/// assemblies. cfg-stripping runs in the compiler frontend before the driver sees
/// `TyCtxt`, so one compile = one config; to cover a cfg you must actually compile
/// under it (`--tests`, `--all-features`, …). A pub item is a lead iff it's a
/// candidate in ≥1 config and reached in **none** — usage under *any* cfg saves
/// it. The cross-config join key is the config-stable `(crate, def_path_str)`
/// identity, **not** `DefPathHash` (which is not cross-config stable — verified
/// 0/475 default-vs-`--test`, same toolchain). Within a config the join is still
/// `DefPathHash` (observer-stable). Two-level, and the dual of §6's cross-crate
/// join: neither key is stable on both the observer and the config axis.
struct Matrix<'a> {
    /// One entry per config; `configs[0]` is the **primary** config — it defines
    /// the member-crate set and the real dependency-leaf tags, so a `--tests`
    /// config's integration-test crates contribute *usage* but never *candidates*.
    /// Borrowed so `main` keeps ownership for the follow-on unused-deps pass.
    configs: &'a [(String, Assembly)],
}

impl Matrix<'_> {
    fn report(&self, meta: Option<&Meta>) {
        let (primary_name, primary) = &self.configs[0];
        let members = &primary.crates;

        // Level-1→2: reduce each config to config-stable candidate identities.
        let per: Vec<(&str, BTreeMap<String, CandReach>)> = self
            .configs
            .iter()
            .map(|(n, a)| (n.as_str(), a.candidate_identities()))
            .collect();

        // Union of reached identities across ALL configs (any crate — a test crate
        // referencing a member is real usage of that member).
        let mut used: BTreeSet<String> = BTreeSet::new();
        for (_, cands) in &per {
            for (id, c) in cands {
                if c.reached {
                    used.insert(id.clone());
                }
            }
        }

        // Union candidate set, restricted to member crates (the real pub API).
        let mut all: BTreeMap<String, &CandReach> = BTreeMap::new();
        for (_, cands) in &per {
            for (id, c) in cands {
                if members.contains(&c.krate) {
                    all.entry(id.clone()).or_insert(c);
                }
            }
        }

        println!(
            "cfg-matrix union — {} configs, primary = `{}` ({} member crates)\n",
            self.configs.len(),
            primary_name,
            members.len(),
        );
        println!("Per-config member-crate candidates (reached within that single cfg):");
        for ((name, cands), (_, asm)) in per.iter().zip(self.configs) {
            let mc: Vec<&CandReach> = cands
                .iter()
                .filter(|(_, c)| members.contains(&c.krate))
                .map(|(_, c)| c)
                .collect();
            let unreached = mc.iter().filter(|c| !c.reached).count();
            println!(
                "  [{name:<10}] {:>2} fragment crates, {:>4} candidates, {:>3} unreached in this cfg",
                asm.crates.len(),
                mc.len(),
                unreached,
            );
        }
        println!();

        // Surviving leads: candidate somewhere, reached nowhere.
        let mut leads: Vec<(&String, &CandReach)> = all
            .iter()
            .filter(|(id, _)| !used.contains(id.as_str()))
            .map(|(id, c)| (id, *c))
            .collect();
        leads.sort_by(|a, b| a.0.cmp(b.0));

        // Retired: a lead in the primary config, but used under another cfg — the
        // over-report the union clears (the poster children like `builtin_assertions`).
        let primary_cands = &per[0].1;
        let mut retired: Vec<(&String, &str)> = Vec::new();
        for (id, c) in primary_cands {
            if members.contains(&c.krate) && !c.reached && used.contains(id) {
                let saver = per
                    .iter()
                    .skip(1)
                    .find(|(_, cs)| cs.get(id).map(|x| x.reached).unwrap_or(false))
                    .map(|(n, _)| *n)
                    .unwrap_or("?");
                retired.push((id, saver));
            }
        }
        retired.sort();

        let primary_unreached = primary_cands
            .iter()
            .filter(|(_, c)| members.contains(&c.krate) && !c.reached)
            .count();
        println!("Union verdict — a pub item is a lead iff unreached in EVERY config:");
        println!("  primary config `{primary_name}` alone: {primary_unreached} leads");
        println!(
            "  cfg-matrix union:            {} leads  ({} retired by usage under another cfg)\n",
            leads.len(),
            retired.len(),
        );

        if !retired.is_empty() {
            println!(
                "Retired by the union ({} — a lead in `{primary_name}`, used under another cfg):",
                retired.len(),
            );
            for (id, saver) in retired.iter().take(16) {
                println!("    {id}   (used under `{saver}`)");
            }
            println!();
        }

        // Step-5 split: partition the survivors by whether their crate's pub API is
        // an external reachability boundary (published lib). A survivor in a
        // non-boundary crate (bin / publish=false) is DEAD — nothing anywhere can
        // reach it. One in a boundary crate is API surface — a root, not dead, but
        // worth a human's over-exposure check.
        let origin = |cat: Category| match cat {
            Category::InherentImpl => "inherent",
            Category::TraitImpl => "trait-impl",
            _ => "mod-level",
        };
        let (dead, surface): (Vec<_>, Vec<_>) = leads
            .iter()
            .partition(|(_, c)| !primary.external_boundary(&c.krate, meta));

        let boundary_src = if meta.is_some() {
            "cargo-metadata publish/target-kind"
        } else {
            "dependency-leaf proxy (no --ws; pass it for publish metadata)"
        };
        println!("Root classification — crate boundary from: {boundary_src}\n");

        println!(
            "DEAD (verdict — unused in every cfg AND no external boundary) — {}:",
            dead.len()
        );
        if dead.is_empty() {
            println!("    none — no provably-dead pub item in the workspace.");
        } else {
            for (id, c) in dead.iter().take(20) {
                println!("    [{}] {:<6} {id}", origin(c.category), c.kind);
            }
        }
        println!();

        println!(
            "PUBLISHED API SURFACE (root — unused in-workspace, but external consumers \
             possible) — {}:",
            surface.len()
        );
        for (id, c) in surface.iter().take(20) {
            println!("    [{}] {:<6} {id}", origin(c.category), c.kind);
        }
        if !surface.is_empty() {
            println!(
                "  (a published library's pub API is expected to have 0 in-workspace uses; \
                 review for over-exposure, not death.)"
            );
        }
        println!();

        println!(
            "  Identity: cross-config join on (crate, def_path_str) — config-stable; within-config\n  \
             join on DefPathHash — observer-stable. Neither key is stable on both axes (§7, dual of §6).\n  \
             Roots: a published-lib pub API is an external boundary (not dead); bin / publish=false is not."
        );
    }
}

/// The **second lint on the same IR** (SPIKE §4 breadth): `unused-deps`. Diffs each
/// member's declared dependencies (`cargo metadata`) against the reference graph —
/// which crates its targets actually reference — and flags a declared dep with
/// **no** edge from any of its owner package's compiled targets, across **every**
/// provided config. Unioned across configs exactly like unused-pub: a dep exercised
/// under *any* cfg is used. Nearly free — it's the same fragments the unused-pub
/// verdict already loaded, read through the `dep_matrix`'s sibling (raw edge
/// targets) instead of the reverse index.
///
/// Judgement scope (honest limits, all surfaced in the output so a reader knows
/// what was and wasn't checked):
///   * **normal** deps — always judged (lib/bin compile in every config);
///   * **dev** deps — judged only when a test/example/bench target was compiled
///     (a `--tests` config present); else reported as not-judged, never flagged;
///   * **build** deps — never judged (`build.rs` isn't lint-passed — no fragment);
///   * **optional** deps — never judged (feature-gated; not compiled unless enabled).
///
/// Facade crates are handled (SPIKE §4): references resolve to the *defining* crate,
/// not the declared facade — `use clap::Parser` edges all point at `clap_builder` —
/// so a declared dep counts as used when the referenced-crate set intersects its
/// resolved dependency **closure** (`clap_builder ∈ closure(clap)`), via [`Meta::dep_closure`].
///
/// Two residual blind spots remain, both **stated in the output**, not hidden:
///   * **macro-only deps** whose expansion names another crate — e.g. a bare
///     `serde_derive` dep used via `#[derive]`, whose generated code references
///     `serde`. The post-expansion HIR walk sees derive-**generated** trait refs (so
///     a normal `serde` dep with `#[derive(Serialize)]` stays live via the emitted
///     `impl serde::Serialize`), but not the proc-macro crate itself.
///   * **side-effect / build-hook deps** with no API surface — e.g. `cargo-husky`,
///     added purely so its build installs git hooks; it is never named in code, so a
///     ref-graph *correctly* sees zero usage, yet it must not be removed.
///
/// `syn-workspace-marker`'s `expansion_uses!` (macros) and an allow-list (side-effect
/// crates) are the production answers; here we flag the residue for a human.
fn report_unused_deps(configs: &[(String, Assembly)], meta: &Meta) {
    // exercised[pkg] = union, over every config and every target the package owns,
    // of the crate-names that target references. A package's dep is declared once
    // but may be used by any target (lib, bin, integration test), so fold each
    // fragment's edges onto its *owning package* (via `target_owner`) before diffing.
    let mut exercised: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut compiled_test_target = false;
    for (_, asm) in configs {
        for frag in &asm.fragments {
            if meta.test_targets.contains(&frag.crate_name) {
                compiled_test_target = true;
            }
            let Some(owner) = meta.target_owner.get(&frag.crate_name) else {
                continue; // a fragment with no matching manifest target — can't
                          // attribute its usage to a package; skip (don't guess).
            };
            let set = exercised.entry(owner.clone()).or_default();
            for e in &frag.references {
                if let Some(to) = e.to.first() {
                    if to != owner {
                        set.insert(to.clone());
                    }
                }
            }
        }
    }

    let cfg_names: Vec<&str> = configs.iter().map(|(n, _)| n.as_str()).collect();
    println!("Unused-deps verdict (declared deps vs the reference graph):");
    println!(
        "  dep source: cargo-metadata (resolved, for facade closures); unioned across {} config(s): {}",
        configs.len(),
        cfg_names.join(", "),
    );
    println!(
        "  dev-deps: {}",
        if compiled_test_target {
            "judged (a test/example/bench target was compiled)"
        } else {
            "NOT judged (no test target compiled — pass a `--tests` config dir)"
        }
    );
    println!(
        "  build-deps: not judged (build.rs isn't lint-passed); optional deps: not judged (feature-gated)\n"
    );

    let mut total_unused = 0usize;
    for member in &meta.members {
        let decls = match meta.declared.get(member) {
            Some(d) if !d.is_empty() => d,
            _ => continue, // no declared deps (e.g. the marker crates) — nothing to say
        };
        let used = exercised.get(member).cloned().unwrap_or_default();

        let (mut n_normal, mut n_dev, mut n_build, mut n_skip) = (0usize, 0usize, 0usize, 0usize);
        let mut unused: Vec<(&str, &str)> = Vec::new(); // (kind label, dep name)
        for d in decls {
            match d.kind {
                DepKind::Normal => n_normal += 1,
                DepKind::Dev => n_dev += 1,
                DepKind::Build => n_build += 1,
            }
            let judgeable = !d.optional
                && match d.kind {
                    DepKind::Normal => true,
                    DepKind::Dev => compiled_test_target,
                    DepKind::Build => false,
                };
            if !judgeable {
                n_skip += 1;
                continue;
            }
            // A dep is exercised iff the referenced-crate set meets its resolved
            // closure — clears facade crates (clap via clap_builder) soundly.
            let exercised = meta.dep_closure(&d.name).iter().any(|c| used.contains(c));
            if !exercised {
                let k = if d.kind == DepKind::Dev { "dev" } else { "normal" };
                unused.push((k, d.name.as_str()));
            }
        }
        unused.sort();

        let boundary = if meta.is_published_lib(member) {
            "published lib"
        } else {
            "bin/internal"
        };
        println!(
            "  {member:<22} {n_normal} normal, {n_dev} dev, {n_build} build declared \
             ({n_skip} not judged: build/optional/dev)  [{boundary}]"
        );
        if unused.is_empty() {
            println!("     → every judged dep is exercised");
        } else {
            for (k, name) in &unused {
                println!("     ✗ UNUSED  {k:<6} {name}   (no edge from any {member} target)");
                total_unused += 1;
            }
        }
    }
    println!(
        "\n  → {total_unused} unused declared dependency(ies) across the workspace's judged set."
    );
    if total_unused > 0 {
        println!(
            "     (each is a real ref-graph absence — facade crates are already cleared via\n      \
             dependency closures. Before removing, confirm it is not a side-effect/build-hook\n      \
             crate (e.g. cargo-husky) or a macro-only dep whose expansion names another crate.)"
        );
    }
    println!();
}
