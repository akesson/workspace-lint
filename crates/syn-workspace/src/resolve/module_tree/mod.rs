//! Tier 2: cross-file module tree assembly.
//!
//! For each crate, starts at every target root (`lib.rs`/`main.rs` and each
//! cargo target's `src_path` — bin/example/test/bench/build-script) and walks
//! every `mod foo;` declaration to its backing file, resolved in the declaring
//! module's *owning directory*:
//!
//! - A **target root** owns its own *containing* directory regardless of its
//!   filename — it is a crate boundary, so its children are siblings
//!   (`foo.rs` / `foo/mod.rs`). (Callers pass this dir explicitly; computing it
//!   from the stem would wrongly resolve e.g. `tests/integration.rs`'s
//!   `mod common;` into `tests/integration/`.)
//! - A file reached *via* a `mod foo;` declaration owns `dir_owning_children`
//!   of itself: `mod.rs` owns its own dir; any other `bar.rs` owns a `bar/`
//!   subdirectory, so its children live under `bar/` (`bar/foo.rs` /
//!   `bar/foo/mod.rs`).
//! - A `#[path = "..."]` override is instead relative to the directory of the
//!   file that contains the `mod` statement — or, when the module sits inside an
//!   inline `mod { … }` block, that directory *plus the inline-module names as
//!   directories* (Rust's two-case rule; see `resolve_mod_file`).
//!
//! Produces a tree of [`Module`] values rooted at the crate root, each
//! populated with the items declared at that scope. Inline `mod foo { ... }`
//! blocks become submodules backed by the same `file` as their parent, and
//! own a deeper `foo/` directory for any file children declared inside them.
//!
//! Documented limitations:
//! - `#[cfg_attr(cond, path = "...")]` is not expanded, and `include!("…")` is
//!   not followed — structural non-goals (no cfg-attr evaluation, no `include!`
//!   expansion), the same class as proc-macro execution.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::doc_fences;
use super::use_tree::{self, UseBinding};
use super::{
    BrokenModDecl, Error, Item, ItemKind, Module, Occurrence, Origin, ResolvedPath, Result,
    SignatureExposure, SourceSpan, Visibility,
};
use crate::macros::annotation::comment_expansion_uses_occurrences;
use crate::macros::autodetect::extract_macro_paths;
use crate::plugins;

pub(crate) mod items;
mod occurrences;
pub(crate) mod signature;
#[cfg(test)]
mod tests;
mod uses;

// Occurrence helper consumed outside this directory (`macros::autodetect`).
pub(crate) use occurrences::consume_path_run;

// Carved-out helpers the walk below calls, brought into scope so the call sites
// read unchanged.
use items::{decl_ident, extract_cfg_feature_names, item_attrs, item_from_syn, sibling_name};
use occurrences::{extract_code_paths, resolve_occurrences_in_place, use_tree_has_glob};
use signature::collect_signature_exposures;
use uses::{
    function_local_use_bindings, pub_glob_reexport_targets, rewrite_sibling_local, scope_from,
};

/// Items, submodules, `use` bindings, broken `mod` declarations,
/// `#[cfg(feature = "...")]` references, and all resolved reference
/// occurrences collected while walking a module.
struct ModuleContents {
    items: Vec<Item>,
    submodules: Vec<Module>,
    use_bindings: Vec<UseBinding>,
    broken_mod_decls: Vec<BrokenModDecl>,
    cfg_features: Vec<String>,
    occurrences: Vec<Occurrence>,
    glob_reexports: Vec<ResolvedPath>,
    signature_exposures: Vec<SignatureExposure>,
    fact_provenance: Vec<plugins::ProvenancedFact>,
}

/// Convert a `proc_macro2::Span` to a [`SourceSpan`] anchored at `file` — the
/// generalized form of the per-site span construction (`byte_range` below /
/// `use_tree::source_span_from_ident`).
pub(crate) fn span_to_source_span(file: &Path, span: proc_macro2::Span) -> SourceSpan {
    let start = span.start();
    SourceSpan {
        file: file.to_path_buf(),
        line: start.line as u32,
        column: start.column as u32,
        byte_range: byte_range(span),
    }
}

/// Build a fully-populated module tree for one crate, using the default
/// marker-crate names for `expansion_uses!` detection. Most callers
/// reach this via [`crate::Workspace::load`] rather than directly.
///
/// Returns an empty placeholder [`Module`] if the crate has neither `lib.rs`
/// nor `main.rs` at the standard location — for non-standard layouts, the
/// caller should pass an explicit entry point to a future variant.
#[cfg(test)]
pub(crate) fn build_crate_tree(manifest_dir: &Path, crate_name: &str) -> Result<Module> {
    let src_dir = manifest_dir.join("src");
    let candidates = [src_dir.join("lib.rs"), src_dir.join("main.rs")];

    let Some(root_file) = candidates.iter().find(|p| p.exists()) else {
        return Ok(empty_root(crate_name));
    };

    let crate_root_path = ResolvedPath::new([crate_name.to_string()]);
    let default_markers = vec![
        "workspace_syn".to_string(),
        "syn_workspace_marker".to_string(),
    ];
    // A crate root owns its containing directory (`src/`): its `mod foo;`
    // children are siblings, not under a `<stem>/` subdir.
    let mod_dir = root_file.parent().unwrap_or(Path::new("."));
    build_module_from_file(
        root_file,
        mod_dir,
        crate_name.to_string(),
        crate_root_path,
        // Crate roots are the crate boundary itself, not a `mod foo;`
        // declaration, so there's no enclosing visibility — Public is the
        // semantically correct default for any external-reachability check.
        Visibility::Public,
        &default_markers,
    )
}

#[cfg(test)]
fn empty_root(crate_name: &str) -> Module {
    Module {
        name: crate_name.to_string(),
        canonical: ResolvedPath::new([crate_name.to_string()]),
        visibility: Visibility::Public,
        items: Vec::new(),
        submodules: Vec::new(),
        use_bindings: Vec::new(),
        broken_mod_decls: Vec::new(),
        cfg_features: Vec::new(),
        occurrences: Vec::new(),
        glob_reexports: Vec::new(),
        signature_exposures: Vec::new(),
        fact_provenance: Vec::new(),
        file: None,
        doctest_crate_refs: HashSet::new(),
    }
}

/// `mod_dir` is the directory in which this file's `mod foo;` declarations are
/// resolved. For a **target/crate root** (the `src_path` of any cargo target,
/// or `lib.rs`/`main.rs`) it is the file's own directory — a root owns its
/// containing directory regardless of filename. For a file reached *via* a
/// `mod foo;` declaration, the caller passes [`dir_owning_children`] of that
/// file (the `foo.rs`-owns-`foo/` convention). Computing it from the file stem
/// here would be wrong for target roots like `tests/integration.rs`.
pub(crate) fn build_module_from_file(
    file_path: &Path,
    mod_dir: &Path,
    mod_name: String,
    canonical: ResolvedPath,
    visibility: Visibility,
    marker_crates: &[String],
) -> Result<Module> {
    let source = std::fs::read_to_string(file_path)?;
    let parsed = syn::parse_file(&source).map_err(|e| Error::Parse {
        path: file_path.to_path_buf(),
        source: e,
    })?;

    // The dependency-free `// workspace-syn: expansion-uses(...)` comment form of
    // an `expansion_uses!` annotation isn't in the `syn` AST, so recover it from
    // the file text here (where `source` is in hand) and seed it into this file's
    // top-level module — raw, so the Phase-B pass in `collect_module_contents`
    // resolves it alongside every other occurrence.
    let comment_occurrences = comment_expansion_uses_occurrences(&source, file_path);

    // A file's own items are at its top level — not inside any inline block of
    // *this* file, even when the file itself was reached via a `mod foo;`.
    let contents = collect_module_contents(
        &parsed.items,
        file_path,
        mod_dir,
        &canonical,
        marker_crates,
        false,
        comment_occurrences,
    )?;

    Ok(Module {
        name: mod_name,
        canonical,
        visibility,
        items: contents.items,
        submodules: contents.submodules,
        use_bindings: contents.use_bindings,
        broken_mod_decls: contents.broken_mod_decls,
        cfg_features: contents.cfg_features,
        occurrences: contents.occurrences,
        glob_reexports: contents.glob_reexports,
        signature_exposures: contents.signature_exposures,
        fact_provenance: contents.fact_provenance,
        file: Some(file_path.to_path_buf()),
        // Scanned once per file (where `source` is in hand); inline submodules
        // share this file and carry an empty set.
        doctest_crate_refs: doc_fences::doc_fence_crate_refs(&source),
    })
}

fn collect_module_contents(
    syn_items: &[syn::Item],
    parent_file: &Path,
    mod_dir: &Path,
    parent_canonical: &ResolvedPath,
    marker_crates: &[String],
    // Whether these items sit inside one or more inline `mod { … }` blocks of the
    // current file. Governs the `#[path]` base in `resolve_mod_file`: a nested
    // `#[path]` anchors at `mod_dir` (which already carries the inline names),
    // while a top-level one anchors at the declaring file's directory. Resets to
    // `false` when a `mod foo;` crosses into a new file (`build_module_from_file`).
    in_inline: bool,
    // Pre-built occurrences to seed this module's reference surface before the
    // syntactic scan — the comment-directive `expansion_uses` form recovered from
    // raw file text by `build_module_from_file`. Only a file's top-level module
    // receives any; inline-`mod` recursion and unit tests pass an empty Vec. They
    // flow through the Phase-B resolution loop below like any other occurrence.
    seed_occurrences: Vec<Occurrence>,
) -> Result<ModuleContents> {
    let scope = scope_from(parent_canonical);
    // Names declared at this module level. A `use foo::Bar;` whose first
    // segment matches one of these refers to a crate-local sibling, not an
    // external crate — see Rust 2018+ path resolution rules. Order in source
    // doesn't matter, so we collect names in one pass before processing use
    // statements.
    let sibling_names: HashSet<String> = syn_items.iter().filter_map(sibling_name).collect();
    // Whether any module-level `use` here carries a glob leaf (`use m::*;`).
    // Gates the bare-ident `GlobCandidate` capture in `extract_code_paths`:
    // without a glob in scope, an unmatched bare ident is always a local or
    // prelude name. Function-local glob imports are not detected — a
    // documented miss, mirroring the module-level-only glob recording below.
    let has_glob_import = syn_items.iter().any(|it| match it {
        syn::Item::Use(item_use) => use_tree_has_glob(&item_use.tree),
        _ => false,
    });

    let mut items = Vec::new();
    let mut submodules = Vec::new();
    let mut use_bindings = Vec::new();
    let mut broken_mod_decls = Vec::new();
    let mut cfg_features: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    // All reference occurrences for this module (seeded comment-directive refs +
    // macro-body + regular-code + glob + extern-crate). Phase B (below) resolves
    // each in place; this Vec is the module's reference surface, stored directly
    // on `Module.occurrences`.
    let mut occurrences: Vec<Occurrence> = seed_occurrences;
    // Targets of `pub use M::*;` glob re-exports declared in this module.
    let mut glob_reexports: Vec<ResolvedPath> = Vec::new();

    // Built once and reused for item-position macro dispatch, the fn-body
    // dispatch, the code-path claim guard, and the per-item local-fact pass below.
    let resolver_plugins = plugins::builtin_plugins();

    for syn_item in syn_items {
        for attr in item_attrs(syn_item) {
            extract_cfg_feature_names(attr, &mut cfg_features);
        }

        // Tier-H usage assertions: scan the item's full attribute subtree (item,
        // field, and variant attrs) for built-in triggers — strum derives,
        // `#[wasm_bindgen_test]`, `#[serde(with = "…")]` — emitting
        // `Origin::Asserted` reference evidence. Inline modules are skipped here
        // and scanned by their own recursive pass below.
        crate::assertions::scan_item(syn_item, parent_file, &mut occurrences);

        // A `#[derive(Routable)]` enum references its `pub fn` components only
        // through code the derive macro generates (never a bare `rsx!` or a
        // `use`), so they're invisible to the token/AST scans below. Capture
        // those component names as bare `Origin::Component` occurrences; the
        // Phase B `DioxusComponentPass` binds them to the same-crate `pub fn`,
        // exactly like a bare `rsx!` component. Gated with the rest of the
        // Dioxus semantics — with `dioxus` off, route enums are ordinary code.
        #[cfg(feature = "dioxus")]
        if let syn::Item::Enum(item_enum) = syn_item {
            occurrences.extend(plugins::dioxus_rsx::route_component_occurrences(
                item_enum,
                parent_file,
            ));
        }

        if let syn::Item::Use(item_use) = syn_item {
            let mut bindings = use_tree::bindings_from_use(item_use, &scope, parent_file);
            for binding in &mut bindings {
                rewrite_sibling_local(binding, parent_canonical, &sibling_names);
            }
            use_bindings.extend(bindings);
        }

        if let syn::Item::Macro(item_macro) = syn_item {
            // Macro lowering is the single Phase-A extension point: the first
            // built-in lowerer that claims this site decides whether to run the
            // baseline token scan, add structured occurrences, or both.
            let site = plugins::MacroSite {
                is_macro_rules: item_macro.ident.is_some(),
                path: &item_macro.mac.path,
                tokens: &item_macro.mac.tokens,
                marker_crates,
            };
            if let Some(plugin) = resolver_plugins.iter().find(|p| p.claims_macro(&site)) {
                // Span for structured occurrences: the macro-invocation site
                // (the plugin AST doesn't expose per-ref spans).
                let mac_span = item_macro
                    .mac
                    .path
                    .segments
                    .first()
                    .map(|s| span_to_source_span(parent_file, s.ident.span()));
                let cx = plugins::LowerCtx {
                    macro_span: mac_span,
                };
                match plugin.lower_macro(&site, &cx) {
                    plugins::Lowered::TokenScan => {
                        extract_macro_paths(
                            item_macro.mac.tokens.clone(),
                            parent_file,
                            &mut occurrences,
                        );
                    }
                    plugins::Lowered::Exact(occs) => occurrences.extend(occs),
                    plugins::Lowered::ScanPlus(occs) => {
                        extract_macro_paths(
                            item_macro.mac.tokens.clone(),
                            parent_file,
                            &mut occurrences,
                        );
                        occurrences.extend(occs);
                    }
                }
            }
        } else if !matches!(syn_item, syn::Item::Use(_) | syn::Item::Mod(_)) {
            // Dispatch `rsx!`-style macros nested *inside* this item (fn bodies,
            // expression position) to structured lowerers. Item-position macros
            // are handled by the branch above; `Use`/`Mod` carry no relevant
            // bodies. Only structured (`ScanPlus`/`Exact`) lowerers contribute —
            // the baseline scan (`extract_code_paths`, below) already covers
            // fn-body macro *tokens*, so a `TokenScan` lowerer would double-count.
            // The only structured lowerer today is the Dioxus `rsx!` one, so this
            // is gated on its feature: with `dioxus` off, no lowerer can
            // contribute and the per-item AST walk is pure waste.
            #[cfg(feature = "dioxus")]
            {
                let mut v = NestedMacroLowering {
                    lowerers: &resolver_plugins,
                    marker_crates,
                    file: parent_file,
                    out: &mut occurrences,
                };
                syn::visit::Visit::visit_item(&mut v, syn_item);
            }
        }

        if let Some(named) = item_from_syn(syn_item, parent_canonical, parent_file) {
            items.push(named);
        }

        if let syn::Item::Mod(item_mod) = syn_item {
            let child_name = item_mod.ident.to_string();
            let mut child_canonical_segs = parent_canonical.segments().to_vec();
            child_canonical_segs.push(child_name.clone());
            let child_canonical = ResolvedPath::new(child_canonical_segs);

            if let Some((_, inline_items)) = &item_mod.content {
                // An inline `mod a { … }` owns a deeper directory: any file
                // child declared inside it resolves in `<mod_dir>/a/`, not
                // `<mod_dir>/`.
                let inline = collect_module_contents(
                    inline_items,
                    parent_file,
                    &mod_dir.join(&child_name),
                    &child_canonical,
                    marker_crates,
                    // Items here are inside this inline block — a nested
                    // `#[path]` anchors at the (now deeper) `mod_dir`.
                    true,
                    // Comment directives are seeded once, onto the file's
                    // top-level module (`build_module_from_file`), not per inline
                    // block.
                    Vec::new(),
                )?;
                // Inline `mod foo { ... }` shares the parent's `file`.
                // Callers that need the AST re-parse the file via
                // `Workspace::parse_file(path)`; we don't cache here.
                submodules.push(Module {
                    name: child_name,
                    canonical: child_canonical,
                    visibility: Visibility::from_syn(&item_mod.vis),
                    items: inline.items,
                    submodules: inline.submodules,
                    use_bindings: inline.use_bindings,
                    broken_mod_decls: inline.broken_mod_decls,
                    cfg_features: inline.cfg_features,
                    occurrences: inline.occurrences,
                    glob_reexports: inline.glob_reexports,
                    signature_exposures: inline.signature_exposures,
                    fact_provenance: inline.fact_provenance,
                    file: Some(parent_file.to_path_buf()),
                    // Inline modules share the file; its doc fences are scanned
                    // once on the file-backed module.
                    doctest_crate_refs: HashSet::new(),
                });
            } else if let Some(child_file) =
                resolve_mod_file(parent_file, mod_dir, item_mod, in_inline)?
            {
                // A file reached via `mod foo;` owns `dir_owning_children` of
                // itself: `foo.rs` owns `foo/`, `foo/mod.rs` owns `foo/`.
                let child_mod_dir = dir_owning_children(&child_file);
                submodules.push(build_module_from_file(
                    &child_file,
                    &child_mod_dir,
                    child_name,
                    child_canonical,
                    Visibility::from_syn(&item_mod.vis),
                    marker_crates,
                )?);
            } else {
                // `mod foo;` with neither inline body nor backing file —
                // record so consumers (e.g. module-tree integrity
                // checks) can flag the dangling declaration.
                broken_mod_decls.push(BrokenModDecl {
                    name: child_name,
                    declared_in: parent_file.to_path_buf(),
                    line: item_mod.mod_token.span.start().line as u32,
                });
            }
        }
    }

    // Function-local `use` statements (inside fn / impl-method bodies, nested
    // blocks) introduce bindings too, but the top-level pass above only sees
    // module-level `use` items. Fold in the nested ones so a crate-local path
    // like `age::BY_NAME` (after a function-local `use crate::…::age;`) resolves
    // instead of being treated as an external `age` crate.
    use_bindings.extend(function_local_use_bindings(
        syn_items,
        &scope,
        parent_file,
        parent_canonical,
        &sibling_names,
    ));

    // Second pass: extract regular-code path references. Done after the main
    // loop so the use_bindings set is complete — references can resolve any
    // use statement in the module regardless of source order. Pushes into the
    // same `occurrences` list (origins Code / GlobUse / ExternCrate).
    for syn_item in syn_items {
        match syn_item {
            // Use produces use_bindings; nested modules contribute their
            // references via their own ModuleContents. But glob imports
            // (`use foo::bar::*;`) don't produce bindings — we record
            // their prefix as a reference so dep-usage analyses still
            // see the crate.
            syn::Item::Use(item_use) => {
                let span = Some(span_to_source_span(parent_file, item_use.use_token.span));
                let targets = use_tree::glob_targets_from_use(item_use, &scope);
                // A `pub use M::*` re-exports every public item of `M`; record the
                // target so the re-export index can exempt those items from
                // narrowing (same as a named `pub use`). Plain `use M::*` only
                // imports — its prefix is recorded as a reference below, not a
                // re-export.
                glob_reexports.extend(pub_glob_reexport_targets(
                    item_use,
                    &targets,
                    parent_canonical,
                    &sibling_names,
                ));
                for target in targets {
                    occurrences.push(Occurrence {
                        segments: target.segments().to_vec(),
                        path: None,
                        span: span.clone(),
                        origin: Origin::GlobUse,
                    });
                }
                continue;
            }
            syn::Item::Mod(_) => continue,
            // Macro bodies claimed by a lowerer already contributed their
            // occurrences in the macro pass. Skip to avoid double-counting them
            // as regular code.
            syn::Item::Macro(item_macro)
                if resolver_plugins.iter().any(|p| {
                    p.claims_macro(&plugins::MacroSite {
                        is_macro_rules: item_macro.ident.is_some(),
                        path: &item_macro.mac.path,
                        tokens: &item_macro.mac.tokens,
                        marker_crates,
                    })
                }) =>
            {
                continue;
            }
            // `extern crate foo [as bar];` is a single-ident reference that
            // wouldn't match the multi-segment scan. Capture explicitly.
            syn::Item::ExternCrate(ec) => {
                let crate_ident = ec.ident.to_string();
                if crate_ident != "self" {
                    occurrences.push(Occurrence {
                        segments: vec![crate_ident],
                        path: None,
                        span: Some(span_to_source_span(parent_file, ec.ident.span())),
                        origin: Origin::ExternCrate,
                    });
                }
                continue;
            }
            _ => {}
        }

        // The item's own declaring ident is a same-module sibling of itself, so
        // the bare-sibling keep-filter would otherwise record it as a reference
        // to itself (e.g. `pub fn foo` → a spurious `crate::foo` self-ref).
        // Skip that one token by its span; real refs (recursion, a sibling's
        // bare reference) sit at other spans and are kept.
        let own_decl = decl_ident(syn_item).map(|id| span_to_source_span(parent_file, id.span()));
        let tokens = quote::ToTokens::to_token_stream(syn_item);
        extract_code_paths(
            tokens,
            &use_bindings,
            &sibling_names,
            has_glob_import,
            parent_file,
            own_decl.as_ref(),
            &mut occurrences,
        );
    }

    // GlobCandidate volume control: the Phase B pass binds by *name*, so a
    // bare name repeated through a module (a test calling the same helper
    // fifty times) carries no extra signal. Keep one occurrence per name.
    if has_glob_import {
        let mut seen: HashSet<String> = HashSet::new();
        occurrences.retain(|o| {
            o.origin != Origin::GlobCandidate
                || o.segments.first().is_some_and(|s| seen.insert(s.clone()))
        });
    }

    // Phase B: resolve every raw occurrence centrally, filling in its canonical
    // `path` in place. The resolved occurrences are this module's reference
    // surface.
    resolve_occurrences_in_place(
        &mut occurrences,
        &scope,
        &use_bindings,
        &sibling_names,
        parent_canonical,
    );

    // Signature-exposure walk + per-item local-fact plugins (builder-attr today),
    // factored into `collect_local_facts`. AST-aware (not token-based like the scan
    // above); reuses the now-complete `use_bindings` / `scope` / `sibling_names` so
    // resolved canonicals match the occurrence graph. See `signature.rs` / `plugins`.
    let local_ctx = plugins::LocalFactCtx {
        scope: &scope,
        siblings: &sibling_names,
        use_bindings: &use_bindings,
        parent_canonical,
        file: parent_file,
    };
    let (signature_exposures, fact_provenance) =
        collect_local_facts(syn_items, &resolver_plugins, &local_ctx);

    Ok(ModuleContents {
        items,
        submodules,
        use_bindings,
        broken_mod_decls,
        cfg_features: cfg_features.into_iter().collect(),
        occurrences,
        glob_reexports,
        signature_exposures,
        fact_provenance,
    })
}

/// Run the AST-aware signature-exposure walk and the per-item `local_facts` plugins
/// (the builder-attr recognizer today) over a module's items. Returns the recorded
/// signature exposures and the provenance of every plugin-contributed fact. Split out
/// of [`collect_module_contents`] to keep that function's complexity in check; the two
/// outputs land on the module's `signature_exposures` / `fact_provenance`.
fn collect_local_facts(
    syn_items: &[syn::Item],
    resolver_plugins: &[Box<dyn plugins::ResolverPlugin>],
    local_ctx: &plugins::LocalFactCtx,
) -> (Vec<SignatureExposure>, Vec<plugins::ProvenancedFact>) {
    let mut signature_exposures: Vec<SignatureExposure> = Vec::new();
    let mut fact_provenance: Vec<plugins::ProvenancedFact> = Vec::new();
    for syn_item in syn_items {
        collect_signature_exposures(syn_item, local_ctx, &mut signature_exposures);
        // Exposures join the signature vec; references would route via `occurrences`
        // (no built-in local-reference producer yet). Every fact's provenance is kept.
        for plugin in resolver_plugins {
            for fact in plugin.local_facts(syn_item, local_ctx) {
                fact_provenance.push(fact.provenance());
                match fact {
                    plugins::Fact::Exposure { exp, .. } => signature_exposures.push(exp),
                    plugins::Fact::Reference { .. } => {}
                }
            }
        }
    }
    (signature_exposures, fact_provenance)
}

/// Visits an item's nested bodies (fn bodies, expression position, …) and
/// dispatches every claimed macro invocation to a structured lowerer, collecting
/// the structured occurrences it emits. This is how fn-body `rsx!` (the realistic
/// position) reaches the lowerer — the item-position branch in the main walk only
/// sees `syn::Item::Macro`. Only [`plugins::Lowered::ScanPlus`] /
/// [`plugins::Lowered::Exact`] contribute; the baseline token scan
/// ([`extract_code_paths`]) already covers fn-body macro *tokens*, so a
/// `TokenScan` lowerer would double-count and is skipped here.
///
/// A macro inside a fn-body-nested `mod`/`fn` is attributed to the enclosing
/// module rather than the nested one — harmless for the same-crate lints this
/// feeds, and a documented non-goal.
#[cfg(feature = "dioxus")]
struct NestedMacroLowering<'a> {
    lowerers: &'a [Box<dyn plugins::ResolverPlugin>],
    marker_crates: &'a [String],
    file: &'a Path,
    out: &'a mut Vec<Occurrence>,
}

#[cfg(feature = "dioxus")]
impl<'ast, 'a> syn::visit::Visit<'ast> for NestedMacroLowering<'a> {
    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        let site = plugins::MacroSite {
            is_macro_rules: false,
            path: &mac.path,
            tokens: &mac.tokens,
            marker_crates: self.marker_crates,
        };
        if let Some(plugin) = self.lowerers.iter().find(|p| p.claims_macro(&site)) {
            let mac_span = mac
                .path
                .segments
                .first()
                .map(|s| span_to_source_span(self.file, s.ident.span()));
            let cx = plugins::LowerCtx {
                macro_span: mac_span,
            };
            match plugin.lower_macro(&site, &cx) {
                plugins::Lowered::ScanPlus(occs) | plugins::Lowered::Exact(occs) => {
                    self.out.extend(occs);
                }
                plugins::Lowered::TokenScan => {}
            }
        }
        syn::visit::visit_macro(self, mac);
    }
}

/// The directory in which a file's `mod foo;` children are resolved.
///
/// Rust's module-file convention: a crate root (`lib.rs`/`main.rs`) and a
/// `mod.rs` own the directory they sit in, so their children are siblings;
/// any other file `foo.rs` owns a `foo/` subdirectory, so *its* children live
/// under `foo/`. The old code always used the parent file's directory, which
/// silently dropped every submodule declared in a non-`mod.rs` file.
fn dir_owning_children(file: &Path) -> PathBuf {
    let parent = file.parent().unwrap_or(Path::new("."));
    match file.file_stem().and_then(|s| s.to_str()) {
        Some("mod") | Some("lib") | Some("main") => parent.to_path_buf(),
        Some(stem) => parent.join(stem),
        None => parent.to_path_buf(),
    }
}

/// Locate the source file backing a `mod foo;` declaration.
///
/// A plain `mod foo;` resolves in `mod_dir` (the declaring module's owning
/// directory — see `dir_owning_children`): `<mod_dir>/foo.rs` then
/// `<mod_dir>/foo/mod.rs`. A `#[path = "..."]` override follows Rust's two-case
/// rule, keyed on `in_inline`:
/// - **not inside an inline block** (top level of the file): relative to the
///   directory of the file that contains the `mod` statement
///   (`parent_file`'s directory).
/// - **inside an inline `mod { … }` block**: relative to the file's owning
///   directory *including the inline-module names as directories* — which is
///   exactly what `mod_dir` already accumulates (it is joined with each inline
///   name on the way down). This holds for both mod-rs files (`src/` + inline)
///   and non-mod-rs files (`dir/stem/` + inline), since `mod_dir` starts from
///   [`dir_owning_children`].
fn resolve_mod_file(
    parent_file: &Path,
    mod_dir: &Path,
    item_mod: &syn::ItemMod,
    in_inline: bool,
) -> Result<Option<PathBuf>> {
    let mod_name = item_mod.ident.to_string();

    if let Some(override_path) = path_attribute(&item_mod.attrs) {
        let base = if in_inline {
            mod_dir
        } else {
            parent_file.parent().unwrap_or(Path::new("."))
        };
        let candidate = base.join(&override_path);
        return Ok(candidate.exists().then_some(candidate));
    }

    let adjacent = mod_dir.join(format!("{mod_name}.rs"));
    if adjacent.exists() {
        return Ok(Some(adjacent));
    }

    let nested = mod_dir.join(&mod_name).join("mod.rs");
    if nested.exists() {
        return Ok(Some(nested));
    }

    Ok(None)
}

/// Read a `#[path = "..."]` value from a list of attributes, ignoring
/// `cfg_attr`-wrapped forms (those land in `known_false_*`).
fn path_attribute(attrs: &[syn::Attribute]) -> Option<String> {
    for attr in attrs {
        if !attr.path().is_ident("path") {
            continue;
        }
        if let syn::Meta::NameValue(nv) = &attr.meta
            && let syn::Expr::Lit(lit) = &nv.value
            && let syn::Lit::Str(s) = &lit.lit
        {
            return Some(s.value());
        }
    }
    None
}

/// Byte range of a `proc_macro2::Span`. The `span-locations` feature on
/// `proc-macro2` exposes `byte_range`, which returns inclusive-exclusive
/// offsets within the source file. Returns `None` for synthetic spans
/// (where `byte_range` is empty), so the resulting `SourceSpan` carries
/// `byte_range: None` rather than a zero-zero sentinel.
pub(crate) fn byte_range(span: proc_macro2::Span) -> Option<std::ops::Range<u32>> {
    let r = span.byte_range();
    if r.start == 0 && r.end == 0 {
        None
    } else {
        Some(r.start as u32..r.end as u32)
    }
}
