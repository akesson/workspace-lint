//! The [`Workspace`]: load orchestration (`module_tree` walk → re-export index →
//! reverse reference indexes) and the loaded state every query reads. The
//! read-only accessors live in the sibling `queries` module; the struct's fields
//! are `pub(super)` so that module can reach them.

use std::path::{Path, PathBuf};

use super::re_export;
use super::{Crate, LoadOptions, LoadWarning, Module, ResolvedPath, Result, Target};

/// The top-level resolved workspace.
#[derive(Debug, Clone)]
pub struct Workspace {
    pub(super) crates: Vec<Crate>,
    pub(super) root: PathBuf,
    /// Parsed root `Cargo.toml`. Carries the `[workspace.dependencies]`
    /// table that consumers like centralized-deps checks query, plus the
    /// raw source bytes for comment-based directive scanners.
    pub(super) root_manifest: crate::manifest::Manifest,
    pub(super) re_exports: re_export::ReExportIndex,
    /// Macro implicit references partitioned by defining crate (code name).
    /// Built eagerly at load time by unioning every module's macro-origin
    /// occurrences ([`Module::macro_refs`]) per crate. Used by
    /// [`Workspace::macro_implicit_refs_for`] to compute per-target-crate
    /// reachability-narrowed sets — a macro defined in crate B only
    /// contributes to the set for crate A if A references B (or B == A).
    pub(super) macro_refs_by_crate:
        std::collections::HashMap<String, std::collections::HashSet<ResolvedPath>>,
    /// External-macro references registered via
    /// [`Workspace::register_external_macro_uses`]. Treated as
    /// workspace-wide because we can't tell from `cargo_metadata` which
    /// crates actually invoke an external macro — broadcasting to all
    /// keeps the model conservative for that specific shape.
    pub(super) external_macro_refs: std::collections::HashSet<ResolvedPath>,
    /// Per-crate set of canonical paths referenced from that crate's code
    /// (unions `use` bindings + every resolved `Occurrence`, all origins). Keyed
    /// by the crate's code name (Cargo-form hyphens replaced with '_').
    /// Built once at load time so consumers don't have to re-walk the tree.
    pub(super) references_by_crate:
        std::collections::HashMap<String, std::collections::HashSet<ResolvedPath>>,
    /// Reverse index: for each canonical path, the set of code-form crate
    /// names that reference it. Built from `references_by_crate` with each
    /// path passed through the `pub use` chain in `re_exports`. Every proper
    /// prefix (length ≥ 2) of a referenced path is credited too: a reference
    /// to `a::b::c` is a use of `a::b` (`Type::assoc_fn()` uses `Type`,
    /// `module::item` uses `module`). Same referrer may appear because
    /// intra-crate refs are retained — callers that want "cross-crate only"
    /// filter on `path.crate_name() != referrer`. Pre-computed so the
    /// re-export resolution runs once regardless of how many consumers
    /// query it.
    pub(super) canonical_refs_by_path:
        std::collections::HashMap<ResolvedPath, std::collections::HashSet<String>>,
    /// Canonical paths (with prefixes, like `canonical_refs_by_path` keys)
    /// referenced from a *sibling target* of their own package — an
    /// integration test, bench, example, or non-primary bin. Those targets
    /// link the package's library as an external crate, so they can only
    /// import `pub` items; consumers (`unused-pub`) treat a sibling-target
    /// reference like a cross-crate one — the item must stay `pub`. See
    /// [`Workspace::referenced_from_sibling_target`].
    pub(super) sibling_target_refs: std::collections::HashSet<ResolvedPath>,
    /// For each canonical type path, the widest visibility at which it appears
    /// in the **public signature surface** of some item (a `pub fn` return /
    /// parameter type, a `pub` field, a trait-impl associated-type value, …),
    /// with prefixes credited like [`Self::canonical_refs_by_path`]. Built from
    /// each module's [`Module::signature_exposures`](crate::Module). Consumed by
    /// [`Workspace::exposed_in_public_signature`], which `unused-pub` uses so it
    /// never narrows a type a more-visible item exposes (E0446 /
    /// `private_interfaces`). In the current build only `Public` exposures are
    /// recorded; the `Visibility` value leaves room for a future precise variant.
    pub(super) signature_exposure:
        std::collections::HashMap<ResolvedPath, crate::resolve::Visibility>,
    /// Flat provenance log for every resolver-plugin-contributed fact — which plugin
    /// asserted it, which rule, and where — gathered at both fold sites (global
    /// `global_facts` + per-module `local_facts`). Never consulted by the lint
    /// pipeline; reserved for a future `--explain`. See [`crate::plugins::Fact`].
    #[allow(dead_code)] // terminal sink: populated + tested; read by a future `--explain`.
    pub(super) fact_provenance: Vec<crate::plugins::ProvenancedFact>,
    /// Per-crate set of crate names referenced inside rust-compiling doc-test
    /// code fences (see [`module_tree`]/`doc_fences`). Keyed by code name, like
    /// [`Self::references_by_crate`]. Kept *separate* from the occurrence-derived
    /// reference graph on purpose: a dependency used only in a doc-test example
    /// is genuinely used (the dependency lint must see it), but doc-test code is
    /// a separate compilation unit, so these refs must not reach `unused-pub`,
    /// `architecture`, or the SCIP projection. Consumed only by
    /// [`Self::doctest_dep_refs`].
    pub(super) doctest_dep_refs_by_crate:
        std::collections::HashMap<String, std::collections::HashSet<String>>,
    /// Non-fatal issues collected during the load (typically auxiliary
    /// targets that failed to parse). The library never prints these;
    /// callers decide whether to surface, log, or ignore them.
    pub(super) warnings: Vec<LoadWarning>,
}

impl Workspace {
    /// Load and resolve a workspace at the given root directory, with
    /// default options. See [`Workspace::load_with_options`] for the
    /// configurable form.
    pub fn load(root: impl AsRef<Path>) -> Result<Self> {
        Self::load_with_options(root, LoadOptions::default())
    }

    /// Load and resolve a workspace, configured via [`LoadOptions`].
    ///
    /// Builds the full model in one pass: workspace discovery via
    /// `cargo_metadata`, per-crate module-tree assembly (Tier 2) which
    /// threads Tier 1 use-bindings into each [`Module`], and a
    /// workspace-wide `pub use` chain index (Tier 2.5).
    pub fn load_with_options(root: impl AsRef<Path>, opts: LoadOptions) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let (root_manifest, crates, warnings) =
            crate::walk::load_members(&root, &opts.marker_crates)?;
        let re_exports = re_export::ReExportIndex::build(&crates);
        let mut macro_refs_by_crate: std::collections::HashMap<
            String,
            std::collections::HashSet<ResolvedPath>,
        > = std::collections::HashMap::new();
        let mut references_by_crate: std::collections::HashMap<
            String,
            std::collections::HashSet<ResolvedPath>,
        > = std::collections::HashMap::new();
        let mut doctest_dep_refs_by_crate: std::collections::HashMap<
            String,
            std::collections::HashSet<String>,
        > = std::collections::HashMap::new();
        // Raw (un-canonicalized) paths referenced from sibling targets —
        // every target of a package other than its primary unit. Expanded
        // with prefixes and canonicalized into `sibling_target_refs` below,
        // after the Phase B passes have contributed their flagged edges.
        let mut sibling_target_raw: std::collections::HashSet<ResolvedPath> =
            std::collections::HashSet::new();
        for krate in &crates {
            if !krate.is_workspace_member {
                continue;
            }
            let code_name = krate.code_name();
            let macro_entry = macro_refs_by_crate.entry(code_name.clone()).or_default();
            let entry = references_by_crate.entry(code_name.clone()).or_default();
            let doc_entry = doctest_dep_refs_by_crate
                .entry(code_name.clone())
                .or_default();
            let primary = krate.lib_or_main().map(|t| t as *const Target);
            // Walk every target (lib/bin/example/test/bench/build-script).
            // Each target's tree was built with the parent crate's code_name
            // as canonical root, so cross-crate references (e.g.
            // `serde::Foo` inside an integration test) attribute correctly
            // to the parent. Intra-target paths like `crate::helpers::foo`
            // become `parent_crate::helpers::foo` — self-references that
            // consumers filter out (a dep analyzer ignores them because
            // they don't match a Cargo.toml dep; a visibility analyzer
            // ignores them because they're same-crate; etc.).
            for target in &krate.targets {
                collect_macro_implicit_refs(&target.root, macro_entry);
                collect_module_references(&target.root, entry);
                collect_doctest_refs(&target.root, doc_entry);
                // A sibling target (integration test, bench, example,
                // non-primary bin) links the package's library as an
                // *external* crate — it can only import `pub` items, so its
                // references mark items that must stay `pub`. The build
                // script is technically not a consumer of the lib at all,
                // but including it is harmless (its refs can't name lib
                // items the lib itself could see anyway).
                if primary != Some(target as *const Target) {
                    collect_module_references(&target.root, &mut sibling_target_raw);
                }
            }
        }
        let mut fact_provenance: Vec<crate::plugins::ProvenancedFact> = Vec::new();
        let resolver_plugins = crate::plugins::builtin_plugins();
        // Global resolver-plugin facts (framework semantics). Each plugin is an
        // independent pure contributor: it reads the resolved member crates and
        // returns facts the mechanical resolver structurally can't produce (e.g. a
        // Dioxus `#[component]` ↔ bare `Foo {}` rsx reference edge). Folding the
        // reference edges into `references_by_crate` *before* the reverse index is
        // built means they flow through `re_exports.canonical()` and `referring_crates`
        // exactly like code references. Order-independent: the merge target is a set,
        // so the union is the same regardless of plugin order.
        for plugin in &resolver_plugins {
            for fact in plugin.global_facts(&crates) {
                fact_provenance.push(fact.provenance());
                match fact {
                    crate::plugins::Fact::Reference {
                        edge:
                            crate::plugins::ContributedRef {
                                from,
                                to,
                                via_sibling_target,
                            },
                        by: _,
                    } => {
                        if via_sibling_target {
                            sibling_target_raw.insert(to.clone());
                        }
                        references_by_crate.entry(from).or_default().insert(to);
                    }
                    // No built-in plugin emits a *global* exposure yet; a future one
                    // folds into the signature index built below (cell wired here).
                    crate::plugins::Fact::Exposure { .. } => {}
                }
            }
        }
        let mut canonical_refs_by_path: std::collections::HashMap<
            ResolvedPath,
            std::collections::HashSet<String>,
        > = std::collections::HashMap::new();
        for (referring_crate, refs) in &references_by_crate {
            for path in refs {
                for canonical in canonical_with_prefixes(&re_exports, path) {
                    canonical_refs_by_path
                        .entry(canonical)
                        .or_default()
                        .insert(referring_crate.clone());
                }
            }
        }
        let mut sibling_target_refs: std::collections::HashSet<ResolvedPath> =
            std::collections::HashSet::new();
        for path in &sibling_target_raw {
            sibling_target_refs.extend(canonical_with_prefixes(&re_exports, path));
        }
        // Aggregate signature exposures over every member's primary unit (the
        // lib/main surface — the only API an intra-crate `pub(crate)` tighten
        // could break). Prefix-credited like `canonical_refs_by_path` so a query
        // matches whether the signature named the type directly, via a module
        // path, or through a `pub use` alias. Keep the most-exposing visibility.
        let mut signature_exposure: std::collections::HashMap<
            ResolvedPath,
            crate::resolve::Visibility,
        > = std::collections::HashMap::new();
        for krate in &crates {
            if !krate.is_workspace_member {
                continue;
            }
            if let Some(target) = krate.lib_or_main() {
                for module in target.root.walk() {
                    for exp in &module.signature_exposures {
                        for canonical in canonical_with_prefixes(&re_exports, &exp.canonical) {
                            let entry = signature_exposure
                                .entry(canonical)
                                .or_insert(exp.enclosing_vis);
                            if exposure_rank(exp.enclosing_vis) > exposure_rank(*entry) {
                                *entry = exp.enclosing_vis;
                            }
                        }
                    }
                }
            }
        }
        // Aggregate per-module resolver-plugin provenance (local facts — the
        // builder-attr exposures today) into the flat workspace log, joining the
        // global-fact provenance gathered above. Inert; for a future `--explain`.
        for krate in &crates {
            if !krate.is_workspace_member {
                continue;
            }
            for module in krate.all_modules() {
                fact_provenance.extend(module.fact_provenance.iter().cloned());
            }
        }
        Ok(Self {
            crates,
            root,
            root_manifest,
            re_exports,
            macro_refs_by_crate,
            external_macro_refs: std::collections::HashSet::new(),
            references_by_crate,
            canonical_refs_by_path,
            sibling_target_refs,
            signature_exposure,
            fact_provenance,
            doctest_dep_refs_by_crate,
            warnings,
        })
    }
}

/// Local visibility ranking used only to keep the most-exposing entry in the
/// signature-exposure index. Deliberately *not* a public ordering on
/// [`Visibility`](crate::resolve::Visibility): the restricted variants
/// (`pub(crate)`/`pub(super)`/`pub(in)`) collapse to one rank because only the
/// `Public` distinction matters to [`Workspace::exposed_in_public_signature`].
fn exposure_rank(v: crate::resolve::Visibility) -> u8 {
    use crate::resolve::Visibility::*;
    match v {
        Public => 2,
        PubCrate | PubSuper | PubIn => 1,
        Private => 0,
    }
}

/// The canonical (re-export-resolved) form of `path`, plus the canonical form
/// of every proper prefix of length ≥ 2. A reference to `a::b::c` is also a
/// use of `a::b` — `Type::assoc_fn()` uses `Type`, `module::item` uses
/// `module` — so reverse indexes built from references credit each prefix
/// too. Length-1 prefixes (bare crate names) are skipped: crate-level
/// reference data lives in `references_by_crate`.
fn canonical_with_prefixes(
    re_exports: &re_export::ReExportIndex,
    path: &ResolvedPath,
) -> Vec<ResolvedPath> {
    let segments = path.segments();
    let mut out = Vec::with_capacity(segments.len().max(2) - 1);
    out.push(re_exports.canonical(path));
    for len in 2..segments.len() {
        out.push(re_exports.canonical(&ResolvedPath::new(segments[..len].to_vec())));
    }
    out
}

fn collect_macro_implicit_refs(module: &Module, out: &mut std::collections::HashSet<ResolvedPath>) {
    for m in module.walk() {
        out.extend(m.macro_refs().cloned());
    }
}

/// Walk a crate's module tree and collect every canonical path it references:
/// `use` bindings (declared imports) plus the resolved `path` of every
/// [`Occurrence`] in the subtree — ALL origins, including `Origin::Macro`. The
/// macro-origin union matters — when crate A's macro body mentions `B::foo`, A
/// genuinely depends on B, so any dep-usage analysis would otherwise wrongly
/// flag B as unused.
///
/// The result populates `Workspace::references_by_crate` once per crate at
/// load time. Note: the per-target-crate set built by
/// [`Workspace::macro_implicit_refs_for`] is a different concept — it's
/// the union of macro-body refs from crates that could plausibly invoke
/// a macro affecting the target crate.
fn collect_module_references(module: &Module, out: &mut std::collections::HashSet<ResolvedPath>) {
    for m in module.walk() {
        out.extend(m.use_bindings.iter().map(|b| b.canonical.clone()));
        // Every resolved occurrence, all origins (incl. Macro): a dep/item used
        // only inside a macro body must still count as referenced.
        out.extend(m.occurrences.iter().filter_map(|o| o.path.clone()));
        // Local-fact reference edges (Tier-H assertion plugins): a strum derive,
        // `#[serde(with = "…")]`, `#[wasm_bindgen_test]` references a path no source
        // syntax names. Off `occurrences` so they stay out of SCIP / `references()`.
        out.extend(m.fact_references.iter().cloned());
    }
}

/// Walk a crate's module tree collecting the crate names referenced inside
/// doc-test code fences (populated per file as [`Module::doctest_crate_refs`]).
/// Used only by the dependency lint — see [`Workspace::doctest_dep_refs`].
fn collect_doctest_refs(module: &Module, out: &mut std::collections::HashSet<String>) {
    for m in module.walk() {
        out.extend(m.doctest_crate_refs.iter().cloned());
    }
}
