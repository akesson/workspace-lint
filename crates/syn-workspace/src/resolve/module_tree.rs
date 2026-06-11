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
    SourceSpan, Visibility,
};
use crate::macros::annotation::comment_expansion_uses_occurrences;
use crate::macros::autodetect::extract_macro_paths;
use crate::plugins;

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
pub fn build_crate_tree(manifest_dir: &Path, crate_name: &str) -> Result<Module> {
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

    // Built once and reused for both item-position dispatch and the fn-body
    // dispatch below.
    let lowerers = plugins::builtin_lowerers();

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
            if let Some(lowerer) = lowerers.iter().find(|l| l.claims(&site)) {
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
                match lowerer.lower(&site, &cx) {
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
                    lowerers: &lowerers,
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
                if plugins::claims_any(&plugins::MacroSite {
                    is_macro_rules: item_macro.ident.is_some(),
                    path: &item_macro.mac.path,
                    tokens: &item_macro.mac.tokens,
                    marker_crates,
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

    Ok(ModuleContents {
        items,
        submodules,
        use_bindings,
        broken_mod_decls,
        cfg_features: cfg_features.into_iter().collect(),
        occurrences,
        glob_reexports,
    })
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
    lowerers: &'a [Box<dyn plugins::MacroLowerer>],
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
        if let Some(lowerer) = self.lowerers.iter().find(|l| l.claims(&site)) {
            let mac_span = mac
                .path
                .segments
                .first()
                .map(|s| span_to_source_span(self.file, s.ident.span()));
            let cx = plugins::LowerCtx {
                macro_span: mac_span,
            };
            match lowerer.lower(&site, &cx) {
                plugins::Lowered::ScanPlus(occs) | plugins::Lowered::Exact(occs) => {
                    self.out.extend(occs);
                }
                plugins::Lowered::TokenScan => {}
            }
        }
        syn::visit::visit_macro(self, mac);
    }
}

/// Candidate-select path references from a regular (non-macro) item body: scan
/// for `Ident :: Ident (:: Ident)*` runs and emit each as a raw `Origin::Code`
/// [`Occurrence`] (segments + span). Resolution — crate/self/super peeling,
/// use-binding substitution, sibling rewrite — happens later and centrally in
/// [`resolve_occurrence`].
///
/// The only resolution-aware decision kept here is candidate SELECTION: a bare
/// single ident is emitted only if it names a `use`-binding (`local_name`) or a
/// same-module sibling item (`sibling_names`) — otherwise it's a local/prelude
/// name and is dropped; multi-segment runs are always emitted. `use_bindings`
/// and `sibling_names` are passed solely for that keep-filter; resolution is
/// deferred to [`resolve_occurrence`] (whose sibling branch turns a kept bare
/// sibling ident into `parent_canonical::Ident`).
///
/// `own_decl` is the span of the scanned item's own declaring ident, if any.
/// Since an item's declaration name is a same-module sibling of itself, the
/// keep-filter would record it as a reference to itself; we drop exactly that
/// one token (matched by span) so a never-used item isn't seen as referencing
/// itself. Genuine refs (recursion, a sibling's bare reference) sit at other
/// spans and are kept.
///
/// Keeping bare sibling names can still record a spurious same-crate ref when a
/// local var/param collides with a sibling *name* (most likely a sibling fn —
/// types are PascalCase). That only ever *suppresses* a lint finding (never
/// invents a reference into another crate, so it can't dent the cross-crate SCIP
/// gate), so the precision-for-recall trade is worth it.
fn extract_code_paths(
    tokens: proc_macro2::TokenStream,
    use_bindings: &[UseBinding],
    sibling_names: &HashSet<String>,
    // When the surrounding module has a glob import (`use m::*;`), bare
    // single idents that match no binding or sibling are kept as
    // `Origin::GlobCandidate` occurrences for the Phase B `GlobImportPass`
    // (most are locals and never bind — see that pass's precision notes).
    keep_bare_for_glob: bool,
    file: &Path,
    own_decl: Option<&SourceSpan>,
    out: &mut Vec<Occurrence>,
) {
    let stream: Vec<proc_macro2::TokenTree> = tokens.into_iter().collect();
    let mut i = 0;
    while i < stream.len() {
        if let proc_macro2::TokenTree::Ident(first) = &stream[i] {
            let mut segments = vec![first.to_string()];
            let mut j = i + 1;
            while let (Some(p1), Some(p2), Some(next)) =
                (stream.get(j), stream.get(j + 1), stream.get(j + 2))
            {
                let (proc_macro2::TokenTree::Punct(a), proc_macro2::TokenTree::Punct(b)) = (p1, p2)
                else {
                    break;
                };
                if a.as_char() != ':' || b.as_char() != ':' {
                    break;
                }
                let proc_macro2::TokenTree::Ident(next) = next else {
                    break;
                };
                segments.push(next.to_string());
                j += 3;
            }
            // A bare single-ident macro *invocation* — `foo!(…)` / `foo![…]` /
            // `foo!{…}`, i.e. `Ident` then `!` then a delimited group — resolves
            // in the macro namespace, where an exported `macro_rules!` is
            // crate-global. The central path resolver doesn't model that, so emit
            // it as `Origin::MacroCall` for the core `MacroCallPass` to bind to a
            // same-crate definition (the macro's args, in the group, are still
            // scanned via the group recursion below). Multi-segment macro paths
            // (`m::foo!`) stay ordinary `Origin::Code` runs handled by the
            // keep-filter, so this never disturbs the macro-vs-crate distinction
            // the sibling exclusion guards (e.g. `log` in `log::debug!`).
            let is_macro_call = segments.len() == 1
                && matches!(stream.get(j), Some(proc_macro2::TokenTree::Punct(p)) if p.as_char() == '!')
                && matches!(stream.get(j + 1), Some(proc_macro2::TokenTree::Group(_)));
            if is_macro_call {
                out.push(Occurrence {
                    segments,
                    path: None,
                    span: Some(span_to_source_span(file, first.span())),
                    origin: Origin::MacroCall,
                });
                i = j;
                continue;
            }
            // Candidate selection only — resolution happens centrally in
            // `resolve_occurrence`. Keep multi-segment runs, plus single idents
            // that match a use-binding's local name (the binding set is needed
            // here, but the substitution itself is deferred to resolution).
            let keep = segments.len() >= 2
                || (segments.len() == 1
                    && (use_bindings.iter().any(|b| b.local_name == segments[0])
                        || sibling_names.contains(&segments[0])));
            // GlobCandidate filter: skip keywords/primitives, idents in
            // field/method position (preceded by `.`), lifetimes (preceded
            // by `'`), and names declaring a binding/field/bound (followed
            // by a lone `:` — a `::` run was already consumed above, and a
            // turbofish's first `:` is Joint, so a lone Alone `:` is left).
            let glob_candidate = !keep
                && keep_bare_for_glob
                && segments.len() == 1
                && is_glob_candidate_name(&segments[0])
                && !matches!(
                    (i > 0).then(|| &stream[i - 1]),
                    Some(proc_macro2::TokenTree::Punct(p)) if p.as_char() == '.' || p.as_char() == '\''
                )
                && !matches!(
                    stream.get(j),
                    Some(proc_macro2::TokenTree::Punct(p))
                        if p.as_char() == ':' && p.spacing() == proc_macro2::Spacing::Alone
                );
            if keep || glob_candidate {
                let occ_span = span_to_source_span(file, first.span());
                // Drop the item's own declaration name — a single-ident
                // occurrence at the declaring ident's span is the definition,
                // not a use of itself.
                let is_own_decl = segments.len() == 1 && own_decl == Some(&occ_span);
                if !is_own_decl {
                    out.push(Occurrence {
                        segments,
                        path: None,
                        span: Some(occ_span),
                        origin: if keep {
                            Origin::Code
                        } else {
                            Origin::GlobCandidate
                        },
                    });
                }
            }
            i = j;
            continue;
        }
        if let proc_macro2::TokenTree::Group(group) = &stream[i] {
            extract_code_paths(
                group.stream(),
                use_bindings,
                sibling_names,
                keep_bare_for_glob,
                file,
                own_decl,
                out,
            );
        }
        i += 1;
    }
}

/// Whether a `use` tree contains a glob leaf anywhere (`use a::b::*;`,
/// `use a::{b, c::*};`). Drives the per-module `GlobCandidate` capture gate
/// in [`collect_module_contents`].
fn use_tree_has_glob(tree: &syn::UseTree) -> bool {
    match tree {
        syn::UseTree::Glob(_) => true,
        syn::UseTree::Path(p) => use_tree_has_glob(&p.tree),
        syn::UseTree::Group(g) => g.items.iter().any(use_tree_has_glob),
        syn::UseTree::Name(_) | syn::UseTree::Rename(_) => false,
    }
}

/// Idents that can never be a glob-imported item name — Rust keywords (strict,
/// reserved, and the contextual ones that show up in token position) plus
/// primitive type names. Filters the `GlobCandidate` capture in
/// [`extract_code_paths`].
fn is_glob_candidate_name(name: &str) -> bool {
    const NON_CANDIDATES: &[&str] = &[
        // Strict keywords.
        "as",
        "break",
        "const",
        "continue",
        "crate",
        "dyn",
        "else",
        "enum",
        "extern",
        "false",
        "fn",
        "for",
        "if",
        "impl",
        "in",
        "let",
        "loop",
        "match",
        "mod",
        "move",
        "mut",
        "pub",
        "ref",
        "return",
        "self",
        "Self",
        "static",
        "struct",
        "super",
        "trait",
        "true",
        "type",
        "unsafe",
        "use",
        "where",
        "while",
        "async",
        "await",
        // Reserved / contextual.
        "abstract",
        "become",
        "box",
        "do",
        "final",
        "macro",
        "override",
        "priv",
        "try",
        "typeof",
        "union",
        "unsized",
        "virtual",
        "yield",
        "macro_rules",
        "raw",
        "_",
        // Primitives.
        "bool",
        "char",
        "str",
        "u8",
        "u16",
        "u32",
        "u64",
        "u128",
        "usize",
        "i8",
        "i16",
        "i32",
        "i64",
        "i128",
        "isize",
        "f32",
        "f64",
    ];
    !NON_CANDIDATES.contains(&name)
}

/// Apply `crate::` / `self::` / `super::` prefix peeling to the leading
/// segments of a path, mutating the iterator in place. The remaining segments
/// (everything after the last consumed `crate`/`self`/`super`) are left in
/// the iterator for the caller to handle.
///
/// Returns `Some((prefix, started))` where `started` is `true` iff at least
/// one leading segment was peeled (in which case the resolver should NOT
/// apply use-binding / sibling lookups to the next segment — those rules
/// only apply to externally-anchored paths). Returns `None` if a `super::`
/// would escape the crate root: rustc errors on those, so we drop the
/// reference rather than misattribute it to `crate::remaining`.
fn peel_path_prefix(
    iter: &mut std::iter::Peekable<std::vec::IntoIter<String>>,
    scope: &use_tree::Scope,
) -> Option<(Vec<String>, bool)> {
    let mut prefix: Vec<String> = Vec::new();
    let mut started = false;

    while let Some(head) = iter.peek().cloned() {
        match head.as_str() {
            "crate" if !started => {
                prefix.clear();
                prefix.push(scope.crate_name.clone());
                started = true;
                iter.next();
            }
            "self" if !started => {
                prefix.clear();
                prefix.push(scope.crate_name.clone());
                prefix.extend(scope.module_path.iter().cloned());
                started = true;
                iter.next();
            }
            "super" => {
                if !started {
                    prefix.push(scope.crate_name.clone());
                    prefix.extend(scope.module_path.iter().cloned());
                    started = true;
                }
                if prefix.len() <= 1 {
                    return None;
                }
                prefix.pop();
                iter.next();
            }
            _ => break,
        }
    }
    Some((prefix, started))
}

/// Resolve a code-path's leading segment through (in order):
/// crate/self/super peeling, use-binding substitution, sibling lookup,
/// then external-crate fallback. See [`extract_code_paths`] for context.
fn resolve_code_path(
    segments: Vec<String>,
    scope: &use_tree::Scope,
    siblings: &HashSet<String>,
    use_bindings: &[UseBinding],
    parent_canonical: &ResolvedPath,
) -> Option<ResolvedPath> {
    if segments.is_empty() {
        return None;
    }
    let mut iter = segments.into_iter().peekable();
    let (mut prefix, started) = peel_path_prefix(&mut iter, scope)?;
    let remaining: Vec<String> = iter.collect();
    if remaining.is_empty() {
        return None;
    }
    let first_remaining = remaining.first()?.clone();

    if !started {
        // Use-binding shadows siblings and externals.
        if let Some(binding) = use_bindings
            .iter()
            .find(|b| b.local_name == first_remaining)
        {
            let mut segs = binding.canonical.segments().to_vec();
            segs.extend(remaining.into_iter().skip(1));
            return Some(ResolvedPath::new(segs));
        }
        if siblings.contains(&first_remaining) {
            let mut segs = parent_canonical.segments().to_vec();
            segs.extend(remaining);
            return Some(ResolvedPath::new(segs));
        }
        // Unmatched single segment — almost always a local var or prelude
        // name, not a cross-crate reference. Drop to avoid noise.
        if remaining.len() == 1 {
            return None;
        }
    }
    prefix.extend(remaining);
    Some(ResolvedPath::new(prefix))
}

/// Apply the same crate/self/super peeling and sibling-prepend logic that
/// `bindings_from_use` does, but for a path extracted from raw tokens.
pub(crate) fn resolve_macro_path(
    segments: Vec<String>,
    scope: &use_tree::Scope,
    siblings: &HashSet<String>,
    parent_canonical: &ResolvedPath,
) -> Option<ResolvedPath> {
    if segments.is_empty() {
        return None;
    }
    let mut iter = segments.into_iter().peekable();
    let (mut prefix, started) = peel_path_prefix(&mut iter, scope)?;
    let remaining: Vec<String> = iter.collect();
    if remaining.is_empty() {
        return None;
    }
    let first_remaining = remaining.first()?.clone();
    if !started && siblings.contains(&first_remaining) {
        // Sibling local — prepend the surrounding module's canonical.
        let mut segs = parent_canonical.segments().to_vec();
        segs.extend(remaining);
        return Some(ResolvedPath::new(segs));
    }
    prefix.extend(remaining);
    Some(ResolvedPath::new(prefix))
}

/// Phase B: fill in each occurrence's canonical `path` in place. An occurrence
/// that already carries a `path` is left as-is — only Tier-H assertions do this,
/// pre-setting an absolute implied path at extraction; re-resolving would route
/// a canonical path back through use-binding substitution. Every other origin
/// arrives with `path == None`, so the guard is behaviour-neutral for them.
fn resolve_occurrences_in_place(
    occurrences: &mut [Occurrence],
    scope: &use_tree::Scope,
    use_bindings: &[UseBinding],
    sibling_names: &HashSet<String>,
    parent_canonical: &ResolvedPath,
) {
    for occ in occurrences {
        if occ.path.is_some() {
            continue;
        }
        occ.path = resolve_occurrence(occ, scope, use_bindings, sibling_names, parent_canonical);
    }
}

/// Canonicalize one raw occurrence — the single home for the crate/self/super
/// peeling + use-binding substitution + sibling rewrite that used to be smeared
/// across the extractors. Dispatches on [`Origin`]: `Code` paths resolve against
/// the surrounding `use` bindings; `Macro` paths resolve at the defining scope
/// (no use-binding substitution); `GlobUse`/`ExternCrate` segments are already
/// resolved at extraction, so they pass through unchanged.
fn resolve_occurrence(
    occ: &Occurrence,
    scope: &use_tree::Scope,
    use_bindings: &[UseBinding],
    siblings: &HashSet<String>,
    parent_canonical: &ResolvedPath,
) -> Option<ResolvedPath> {
    match occ.origin {
        Origin::Code => resolve_code_path(
            occ.segments.clone(),
            scope,
            siblings,
            use_bindings,
            parent_canonical,
        ),
        Origin::Macro => {
            resolve_macro_path(occ.segments.clone(), scope, siblings, parent_canonical)
        }
        // Deferred to a Phase B `ResolvePass`: a bare framework-component name
        // carries no scope the central resolver can use without wrongly binding it.
        Origin::Component => None,
        // Deferred to the core Phase B `MacroCallPass`: a bare macro invocation
        // `foo!(…)` resolves in the *macro* namespace (crate-global for an
        // exported `macro_rules!`), which path resolution doesn't model.
        Origin::MacroCall => None,
        // Deferred to the core Phase B `GlobImportPass`: a bare ident in a
        // glob-importing module can only be bound once the glob target's
        // module tree exists.
        Origin::GlobCandidate => None,
        // A Tier-H assertion's *relative* implied path (an `#[serde(with =
        // "routes")]` string, say) resolves like ordinary code: a `use`
        // binding, a `crate`/`self`/`super` prefix, or a same-module sibling.
        // A form `resolve_code_path` drops (a bare external single segment such
        // as `with = "serde_bytes"`) falls back to its raw segments as an
        // already-resolved path, so the leading segment still credits the dep.
        // Over-crediting is FP-safe by the §13 tier contract — a wrongly bound
        // segment only fails to suppress, it can never create a finding.
        // (Absolute implied paths are pre-set at extraction and never reach
        // here — see the `occ.path.is_some()` guard in Phase B.)
        Origin::Asserted { .. } => resolve_code_path(
            occ.segments.clone(),
            scope,
            siblings,
            use_bindings,
            parent_canonical,
        )
        .or_else(|| Some(ResolvedPath::new(occ.segments.clone()))),
        Origin::GlobUse | Origin::ExternCrate => Some(ResolvedPath::new(occ.segments.clone())),
    }
}

/// Outer attributes of a syn item. Returned as a slice so the caller can
/// iterate without copying.
fn item_attrs(item: &syn::Item) -> &[syn::Attribute] {
    match item {
        syn::Item::Const(i) => &i.attrs,
        syn::Item::Enum(i) => &i.attrs,
        syn::Item::ExternCrate(i) => &i.attrs,
        syn::Item::Fn(i) => &i.attrs,
        syn::Item::ForeignMod(i) => &i.attrs,
        syn::Item::Impl(i) => &i.attrs,
        syn::Item::Macro(i) => &i.attrs,
        syn::Item::Mod(i) => &i.attrs,
        syn::Item::Static(i) => &i.attrs,
        syn::Item::Struct(i) => &i.attrs,
        syn::Item::Trait(i) => &i.attrs,
        syn::Item::TraitAlias(i) => &i.attrs,
        syn::Item::Type(i) => &i.attrs,
        syn::Item::Union(i) => &i.attrs,
        syn::Item::Use(i) => &i.attrs,
        syn::Item::Verbatim(_) => &[],
        _ => &[],
    }
}

/// Scan an attribute for `feature = "name"` predicates inside `cfg(...)` or
/// `cfg_attr(<cfg>, ...)`. Predicates can be nested under `any(...)`,
/// `all(...)`, and `not(...)`; we recurse through the meta-list tree.
fn extract_cfg_feature_names(attr: &syn::Attribute, out: &mut std::collections::BTreeSet<String>) {
    let ident = match attr.path().get_ident() {
        Some(i) => i.to_string(),
        None => return,
    };
    if ident != "cfg" && ident != "cfg_attr" {
        return;
    }
    // Parse the inner meta. cfg(...) and cfg_attr(<cfg>, ...) both start
    // with a Meta::List whose nested predicate-tree we scan.
    if let syn::Meta::List(list) = &attr.meta {
        scan_cfg_tokens(list.tokens.clone(), out);
    }
}

fn scan_cfg_tokens(tokens: proc_macro2::TokenStream, out: &mut std::collections::BTreeSet<String>) {
    let iter: Vec<proc_macro2::TokenTree> = tokens.into_iter().collect();
    let mut i = 0;
    while i < iter.len() {
        if let proc_macro2::TokenTree::Ident(id) = &iter[i] {
            let name = id.to_string();
            if name == "feature"
                && let Some(proc_macro2::TokenTree::Punct(p)) = iter.get(i + 1)
                && p.as_char() == '='
                && let Some(proc_macro2::TokenTree::Literal(lit)) = iter.get(i + 2)
            {
                let s = lit.to_string();
                let trimmed = s.trim_matches('"');
                if !trimmed.is_empty() {
                    out.insert(trimmed.to_string());
                }
                i += 3;
                continue;
            }
        }
        if let proc_macro2::TokenTree::Group(g) = &iter[i] {
            scan_cfg_tokens(g.stream(), out);
        }
        i += 1;
    }
}

/// The ident that *declares* an item, when the item introduces a single name —
/// the `fn`/`struct`/`enum`/… name. Returns a borrow so callers can read its
/// span (the declaration site) as well as its text. The single source of truth
/// for both [`sibling_name`] (the module's lexical name set) and the
/// declaration-site skip in [`extract_code_paths`].
fn decl_ident(item: &syn::Item) -> Option<&syn::Ident> {
    match item {
        syn::Item::Fn(i) => Some(&i.sig.ident),
        syn::Item::Struct(i) => Some(&i.ident),
        syn::Item::Enum(i) => Some(&i.ident),
        syn::Item::Union(i) => Some(&i.ident),
        syn::Item::Trait(i) => Some(&i.ident),
        syn::Item::Type(i) => Some(&i.ident),
        syn::Item::Const(i) => Some(&i.ident),
        syn::Item::Static(i) => Some(&i.ident),
        syn::Item::Mod(i) => Some(&i.ident),
        // A `macro_rules!` definition introduces a name in the *macro*
        // namespace only. A path-position reference like `log::debug` — or a
        // bare type/value ident — resolves in the type/value/module namespace,
        // where the macro name does not participate, so a macro must NOT be a
        // sibling that shadows an external-crate reference of the same name
        // (e.g. memchr's `macro_rules! log` vs. the `log` crate, where
        // `log::debug!` inside another macro is the only use of `log`). A name
        // that is *also* a module/type/etc. still enters via that item's arm.
        syn::Item::Macro(_) => None,
        _ => None,
    }
}

/// Names declared at a module's lexical scope — function/struct/enum/etc.
/// idents plus child module names. Used to distinguish "crate-local sibling"
/// from "external crate" at the leading segment of a `use` path.
fn sibling_name(item: &syn::Item) -> Option<String> {
    decl_ident(item).map(|ident| ident.to_string())
}

/// Canonicalized targets of a `pub use M::*;` glob re-export, or empty for a
/// private (`use M::*;`) glob — private globs import, they don't re-export.
fn pub_glob_reexport_targets(
    item_use: &syn::ItemUse,
    targets: &[ResolvedPath],
    parent_canonical: &ResolvedPath,
    sibling_names: &HashSet<String>,
) -> Vec<ResolvedPath> {
    if !matches!(Visibility::from_syn(&item_use.vis), Visibility::Public) {
        return Vec::new();
    }
    targets
        .iter()
        .map(|t| canonicalize_glob_target(t, parent_canonical, sibling_names))
        .collect()
}

/// Canonicalize a glob re-export target prefix to a full module path.
/// `glob_targets_from_use` already peels `crate::`/`self::`/`super::` (those
/// land with the crate name as the leading segment), but a bare leading segment
/// that names a sibling module — `pub use inner::*` — still needs this module's
/// canonical prepended. A leading segment equal to the crate name is already
/// anchored; anything else (an external crate) is left as-is.
fn canonicalize_glob_target(
    target: &ResolvedPath,
    parent_canonical: &ResolvedPath,
    siblings: &HashSet<String>,
) -> ResolvedPath {
    let crate_name = parent_canonical.segments().first();
    match target.segments().first() {
        Some(first) if Some(first) != crate_name && siblings.contains(first) => {
            let mut segs = parent_canonical.segments().to_vec();
            segs.extend(target.segments().iter().cloned());
            ResolvedPath::new(segs)
        }
        _ => target.clone(),
    }
}

/// Collects `use` statements nested inside item bodies (fn / impl-method
/// blocks, nested blocks) so their bindings can resolve later paths in the same
/// module. Deliberately does **not** descend into nested `mod` items — those own
/// their own scope and are resolved by the submodule recursion in
/// [`collect_module_contents`].
#[derive(Default)]
struct NestedUseCollector<'ast> {
    uses: Vec<&'ast syn::ItemUse>,
}

impl<'ast> syn::visit::Visit<'ast> for NestedUseCollector<'ast> {
    fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
        self.uses.push(node);
    }

    fn visit_item_mod(&mut self, _node: &'ast syn::ItemMod) {
        // Stop: a nested module's `use`s belong to *its* scope, and the file/
        // inline-module recursion already processes them there.
    }
}

/// Bindings from every `use` statement nested inside an item body (fn /
/// impl-method blocks). Module-scoped — a slight over-approximation that only
/// *adds* references the code already makes (it can't invent a cross-crate ref
/// out of nothing), so the cross-crate SCIP precision gate is unaffected,
/// mirroring the sibling-name broadening. Module-level `use` items are handled
/// in the main pass; nested `mod`s carry their own scope and are skipped here.
fn function_local_use_bindings(
    syn_items: &[syn::Item],
    scope: &use_tree::Scope,
    parent_file: &Path,
    parent_canonical: &ResolvedPath,
    sibling_names: &HashSet<String>,
) -> Vec<UseBinding> {
    let mut out = Vec::new();
    for syn_item in syn_items {
        if matches!(syn_item, syn::Item::Use(_) | syn::Item::Mod(_)) {
            continue;
        }
        let mut collector = NestedUseCollector::default();
        syn::visit::Visit::visit_item(&mut collector, syn_item);
        for item_use in collector.uses {
            let mut bindings = use_tree::bindings_from_use(item_use, scope, parent_file);
            for binding in &mut bindings {
                rewrite_sibling_local(binding, parent_canonical, sibling_names);
            }
            out.extend(bindings);
        }
    }
    out
}

/// If `binding`'s canonical path starts with a name that's declared in the
/// surrounding module (a sibling), prepend the surrounding module's path so
/// the canonical resolves crate-local instead of being treated as an
/// external crate.
fn rewrite_sibling_local(
    binding: &mut UseBinding,
    parent_canonical: &ResolvedPath,
    siblings: &HashSet<String>,
) {
    let Some(first) = binding.canonical.segments().first() else {
        return;
    };
    if !siblings.contains(first) {
        return;
    }
    let mut new_segs = parent_canonical.segments().to_vec();
    new_segs.extend(binding.canonical.segments().iter().cloned());
    binding.canonical = ResolvedPath::new(new_segs);
}

fn scope_from(canonical: &ResolvedPath) -> use_tree::Scope {
    let segs = canonical.segments();
    let crate_name = segs.first().cloned().unwrap_or_default();
    let module_path = segs.get(1..).map(<[String]>::to_vec).unwrap_or_default();
    use_tree::Scope {
        crate_name,
        module_path,
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

fn item_from_syn(item: &syn::Item, parent_canonical: &ResolvedPath, file: &Path) -> Option<Item> {
    // Full span of the item, used by callers that need to rewrite the
    // item structurally (e.g. visibility tighteners, dead-code removers).
    let full_span = match item {
        syn::Item::Fn(i) => Some(syn::spanned::Spanned::span(i)),
        syn::Item::Struct(i) => Some(syn::spanned::Spanned::span(i)),
        syn::Item::Enum(i) => Some(syn::spanned::Spanned::span(i)),
        syn::Item::Union(i) => Some(syn::spanned::Spanned::span(i)),
        syn::Item::Trait(i) => Some(syn::spanned::Spanned::span(i)),
        syn::Item::Type(i) => Some(syn::spanned::Spanned::span(i)),
        syn::Item::Const(i) => Some(syn::spanned::Spanned::span(i)),
        syn::Item::Static(i) => Some(syn::spanned::Spanned::span(i)),
        syn::Item::Mod(i) => Some(syn::spanned::Spanned::span(i)),
        syn::Item::Macro(i) => Some(syn::spanned::Spanned::span(i)),
        _ => None,
    };
    let item_byte_range = full_span.and_then(byte_range);

    let (name, kind, vis, line) = match item {
        syn::Item::Fn(i) => (
            i.sig.ident.to_string(),
            ItemKind::Fn,
            &i.vis,
            i.sig.ident.span().start().line,
        ),
        syn::Item::Struct(i) => (
            i.ident.to_string(),
            ItemKind::Struct,
            &i.vis,
            i.ident.span().start().line,
        ),
        syn::Item::Enum(i) => (
            i.ident.to_string(),
            ItemKind::Enum,
            &i.vis,
            i.ident.span().start().line,
        ),
        syn::Item::Union(i) => (
            i.ident.to_string(),
            ItemKind::Union,
            &i.vis,
            i.ident.span().start().line,
        ),
        syn::Item::Trait(i) => (
            i.ident.to_string(),
            ItemKind::Trait,
            &i.vis,
            i.ident.span().start().line,
        ),
        syn::Item::Type(i) => (
            i.ident.to_string(),
            ItemKind::TypeAlias,
            &i.vis,
            i.ident.span().start().line,
        ),
        syn::Item::Const(i) => (
            i.ident.to_string(),
            ItemKind::Const,
            &i.vis,
            i.ident.span().start().line,
        ),
        syn::Item::Static(i) => (
            i.ident.to_string(),
            ItemKind::Static,
            &i.vis,
            i.ident.span().start().line,
        ),
        syn::Item::Mod(i) => (
            i.ident.to_string(),
            ItemKind::Module,
            &i.vis,
            i.ident.span().start().line,
        ),
        syn::Item::Macro(i) => {
            // `macro_rules!` definitions; only emit if named.
            let name = i.ident.as_ref()?.to_string();
            // `macro_rules!` has no `pub` token at the syn level — exports go
            // via `#[macro_export]` attribute. Treat exported macros as
            // Public, others as Private.
            let exported = i.attrs.iter().any(|a| a.path().is_ident("macro_export"));
            let vis = if exported {
                Visibility::Public
            } else {
                Visibility::Private
            };
            let mut canonical = parent_canonical.segments().to_vec();
            canonical.push(name.clone());
            return Some(Item {
                name,
                kind: ItemKind::Macro,
                visibility: vis,
                canonical: ResolvedPath::new(canonical),
                source: Some(SourceSpan {
                    file: file.to_path_buf(),
                    line: i.ident.as_ref().unwrap().span().start().line as u32,
                    column: 1,
                    byte_range: item_byte_range.clone(),
                }),
                // Macros don't expose a `pub` token; visibility is governed
                // by `#[macro_export]` instead, so structural-fix consumers
                // have nothing to rewrite here.
                vis_byte_range: None,
            });
        }
        _ => return None,
    };

    // For public items, capture the byte range of the `pub` keyword itself.
    // Structural-fix consumers narrow `pub` to `pub(crate)` (etc.) by
    // overwriting that range — no scanning past preceding doc comments
    // or attributes required.
    let vis_byte_range = match vis {
        syn::Visibility::Public(token) => byte_range(token.span),
        _ => None,
    };
    let mut canonical = parent_canonical.segments().to_vec();
    canonical.push(name.clone());
    Some(Item {
        name,
        kind,
        visibility: Visibility::from_syn(vis),
        canonical: ResolvedPath::new(canonical),
        source: Some(SourceSpan {
            file: file.to_path_buf(),
            line: line as u32,
            column: 1,
            byte_range: item_byte_range,
        }),
        vis_byte_range,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_dir(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    #[test]
    fn flat_lib_collects_top_level_items() {
        let root = build_crate_tree(&manifest_dir("flat_lib"), "flat_lib").expect("build");
        let names: Vec<_> = root.items.iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"public_fn"), "got {names:?}");
        assert!(names.contains(&"PrivateStruct"), "got {names:?}");
    }

    #[test]
    fn mod_decl_walks_to_adjacent_file() {
        let root =
            build_crate_tree(&manifest_dir("nested_modules"), "nested_modules").expect("build");
        let sub = root
            .submodules
            .iter()
            .find(|m| m.name == "sub")
            .expect("sub mod");
        let item_names: Vec<_> = sub.items.iter().map(|i| i.name.as_str()).collect();
        assert!(item_names.contains(&"child_item"), "got {item_names:?}");
        assert_eq!(sub.canonical.display(), "nested_modules::sub");
    }

    #[test]
    fn mod_decl_walks_to_dir_mod_rs() {
        let root =
            build_crate_tree(&manifest_dir("nested_modules"), "nested_modules").expect("build");
        let dir_mod = root
            .submodules
            .iter()
            .find(|m| m.name == "dir_mod")
            .expect("dir_mod");
        let item_names: Vec<_> = dir_mod.items.iter().map(|i| i.name.as_str()).collect();
        assert!(item_names.contains(&"in_dir_mod"), "got {item_names:?}");
    }

    #[test]
    fn target_root_resolves_sibling_submodule() {
        // Regression guard (follow-up to #29): a target root whose filename
        // stem is not lib/main/mod — e.g. an integration-test root
        // `tests/it.rs` — owns its *containing* directory, so `mod common;`
        // resolves to the sibling `common/mod.rs`, not `it/common/mod.rs`.
        // `walk.rs` builds every target this way; before the fix, every test /
        // example / bench / build-script root silently dropped its submodules.
        let dir = manifest_dir("nested_modules").join("target_root");
        let root = build_module_from_file(
            &dir.join("it.rs"),
            &dir,
            "it".to_string(),
            ResolvedPath::new(["it".to_string()]),
            Visibility::Public,
            &default_markers(),
        )
        .expect("build target root");
        let common = root
            .submodules
            .iter()
            .find(|m| m.name == "common")
            .expect("`mod common;` in a target root must resolve to the sibling common/mod.rs");
        let item_names: Vec<_> = common.items.iter().map(|i| i.name.as_str()).collect();
        assert!(item_names.contains(&"helper"), "got {item_names:?}");
        assert!(
            root.broken_mod_decls.is_empty(),
            "no broken mod decls expected, got {:?}",
            root.broken_mod_decls
        );
    }

    #[test]
    fn file_module_owns_subdir() {
        // `src/sub.rs` declares `pub mod leaf;`; because `sub.rs` is not a
        // `mod.rs`/`lib.rs`, the child lives at `src/sub/leaf.rs`, not
        // `src/leaf.rs`. The old `parent_file.parent()` logic dropped it.
        let root =
            build_crate_tree(&manifest_dir("nested_modules"), "nested_modules").expect("build");
        let sub = root
            .submodules
            .iter()
            .find(|m| m.name == "sub")
            .expect("sub mod");
        let leaf = sub
            .submodules
            .iter()
            .find(|m| m.name == "leaf")
            .expect("sub::leaf should resolve to src/sub/leaf.rs");
        let item_names: Vec<_> = leaf.items.iter().map(|i| i.name.as_str()).collect();
        assert!(item_names.contains(&"in_sub_leaf"), "got {item_names:?}");
        assert_eq!(leaf.canonical.display(), "nested_modules::sub::leaf");
    }

    #[test]
    fn inline_mod_in_file_module_resolves_nested_dir() {
        // `src/sub.rs` has an inline `mod wrap { mod nested; }`. The inline
        // `wrap` owns a deeper dir, so the file child resolves at
        // `src/sub/wrap/nested.rs` — exercising the `mod_dir.join(inline)`
        // threading, not just the file-stem rule.
        let root =
            build_crate_tree(&manifest_dir("nested_modules"), "nested_modules").expect("build");
        let sub = root.submodules.iter().find(|m| m.name == "sub").unwrap();
        let wrap = sub
            .submodules
            .iter()
            .find(|m| m.name == "wrap")
            .expect("inline wrap mod");
        let nested = wrap
            .submodules
            .iter()
            .find(|m| m.name == "nested")
            .expect("wrap::nested should resolve to src/sub/wrap/nested.rs");
        let item_names: Vec<_> = nested.items.iter().map(|i| i.name.as_str()).collect();
        assert!(item_names.contains(&"in_wrap_nested"), "got {item_names:?}");
        assert_eq!(
            nested.canonical.display(),
            "nested_modules::sub::wrap::nested"
        );
    }

    #[test]
    fn path_attribute_overrides_resolution() {
        let root = build_crate_tree(&manifest_dir("path_attr"), "path_attr").expect("build");
        let renamed = root
            .submodules
            .iter()
            .find(|m| m.name == "renamed")
            .expect("renamed submodule");
        let names: Vec<_> = renamed.items.iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"actually_in_other_file"), "got {names:?}");
    }

    #[test]
    fn inline_mod_becomes_submodule_with_same_file() {
        let root = build_crate_tree(&manifest_dir("inline_mod"), "inline_mod").expect("build");
        let inner = root
            .submodules
            .iter()
            .find(|m| m.name == "inner")
            .expect("inline submodule");
        assert_eq!(inner.file, root.file, "inline mod shares parent file");
        let names: Vec<_> = inner.items.iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"nested_fn"), "got {names:?}");
    }

    #[test]
    fn visibility_is_extracted_per_item() {
        let root = build_crate_tree(&manifest_dir("flat_lib"), "flat_lib").expect("build");
        let pub_fn = root.items.iter().find(|i| i.name == "public_fn").unwrap();
        let priv_struct = root
            .items
            .iter()
            .find(|i| i.name == "PrivateStruct")
            .unwrap();
        let pub_crate_const = root.items.iter().find(|i| i.name == "INTERNAL").unwrap();
        assert_eq!(pub_fn.visibility, Visibility::Public);
        assert_eq!(priv_struct.visibility, Visibility::Private);
        assert_eq!(pub_crate_const.visibility, Visibility::PubCrate);
    }

    #[test]
    fn missing_mod_target_is_recorded_as_broken() {
        // `mod ghost;` with no file should not panic; the resolver records a
        // BrokenModDecl entry on the parent module so consumers can flag
        // the dangling declaration.
        let root = build_crate_tree(&manifest_dir("missing_mod"), "missing_mod").expect("build");
        assert!(root.submodules.iter().all(|m| m.name != "ghost"));
        assert!(
            root.broken_mod_decls.iter().any(|d| d.name == "ghost"),
            "expected `ghost` to be recorded as a broken mod decl, got: {:?}",
            root.broken_mod_decls,
        );
    }

    // --- code-path extraction (regular non-macro item bodies) ---

    fn parse_items(src: &str) -> Vec<syn::Item> {
        syn::parse_file(src).expect("valid file").items
    }

    fn default_markers() -> Vec<String> {
        vec!["workspace_syn".into(), "syn_workspace_marker".into()]
    }

    fn collect_refs(src: &str, crate_name: &str) -> Vec<String> {
        let parent_canonical = ResolvedPath::new([crate_name.to_string()]);
        let items = parse_items(src);
        let markers = default_markers();
        let contents = collect_module_contents(
            &items,
            std::path::Path::new("<test>"),
            std::path::Path::new("<test>"),
            &parent_canonical,
            &markers,
            false,
            Vec::new(),
        )
        .expect("collect");
        let out: std::collections::BTreeSet<String> = contents
            .occurrences
            .iter()
            .filter(|o| o.origin != Origin::Macro)
            .filter_map(|o| o.path.as_ref())
            .map(|p| p.display())
            .collect();
        out.into_iter().collect()
    }

    /// Bare-name segments of `Origin::MacroCall` occurrences — the macro
    /// invocations the core `MacroCallPass` later binds to same-crate
    /// `macro_rules!` definitions.
    fn macro_call_names(src: &str) -> Vec<String> {
        let parent_canonical = ResolvedPath::new(["demo".to_string()]);
        let items = parse_items(src);
        let markers = default_markers();
        let contents = collect_module_contents(
            &items,
            std::path::Path::new("<test>"),
            std::path::Path::new("<test>"),
            &parent_canonical,
            &markers,
            false,
            Vec::new(),
        )
        .expect("collect");
        contents
            .occurrences
            .iter()
            .filter(|o| o.origin == Origin::MacroCall)
            .map(|o| o.segments.join("::"))
            .collect()
    }

    #[test]
    fn bare_macro_invocation_is_captured_as_macrocall() {
        // A bare single-ident macro invocation is captured for the MacroCallPass
        // to bind to a same-crate `macro_rules!`; this is what stops an exported
        // macro used only intra-crate from being flagged `unused-pub`.
        let names = macro_call_names("fn f() { my_macro!(Thing); }");
        assert!(names.contains(&"my_macro".to_string()), "got {names:?}");
    }

    #[test]
    fn multi_segment_macro_invocation_is_not_macrocall() {
        // `m::foo!()` is an ordinary multi-segment Origin::Code run, not a bare
        // MacroCall, and the qualified path is still seen as a code reference.
        let names = macro_call_names("fn f() { serde_json::json!({}); }");
        assert!(names.is_empty(), "got {names:?}");
        let refs = collect_refs("fn f() { serde_json::json!({}); }", "demo");
        assert!(
            refs.contains(&"serde_json::json".to_string()),
            "got {refs:?}"
        );
    }

    #[test]
    fn path_segment_before_bang_is_not_a_macrocall() {
        // `log::debug!(...)`: `log` is a crate/module segment (followed by `::`),
        // not a bare macro name — it must stay a code path to the external crate,
        // never mistaken for a local macro invocation (the increment-6 invariant
        // that a `macro_rules! log` must not shadow the `log` crate).
        let names = macro_call_names("fn f() { log::debug!(\"x\"); }");
        assert!(!names.iter().any(|n| n == "log"), "got {names:?}");
        let refs = collect_refs("fn f() { log::debug!(\"x\"); }", "demo");
        assert!(refs.contains(&"log::debug".to_string()), "got {refs:?}");
    }

    #[test]
    fn code_path_extracts_fully_qualified_external() {
        let refs = collect_refs("fn f() { let _ = std::env::args(); }", "demo");
        assert!(refs.contains(&"std::env::args".to_string()), "got {refs:?}");
    }

    #[test]
    fn code_path_substitutes_use_binding() {
        let refs = collect_refs("use other::Bar; fn f() -> Bar { Bar::new() }", "demo");
        assert!(refs.contains(&"other::Bar".to_string()), "got {refs:?}");
        assert!(
            refs.contains(&"other::Bar::new".to_string()),
            "got {refs:?}"
        );
    }

    #[test]
    fn code_path_substitutes_renamed_use() {
        // `use foo::Bar as Baz; Baz::method()` → canonical foo::Bar::method
        let refs = collect_refs("use foo::Bar as Baz; fn f() { Baz::method(); }", "demo");
        assert!(
            refs.contains(&"foo::Bar::method".to_string()),
            "got {refs:?}"
        );
    }

    #[test]
    fn code_path_resolves_crate_prefix() {
        let refs = collect_refs("fn f() { crate::inner::go(); }", "demo");
        assert!(
            refs.contains(&"demo::inner::go".to_string()),
            "got {refs:?}"
        );
    }

    #[test]
    fn code_path_resolves_sibling_local() {
        let refs = collect_refs(
            "fn helper() {} fn f() { helper(); helper::Sub::go(); }",
            "demo",
        );
        // First segment of `helper::Sub::go` resolves crate-local.
        assert!(
            refs.contains(&"demo::helper::Sub::go".to_string()),
            "got {refs:?}"
        );
        // A *bare* single-ident sibling call (`helper()`) is also recorded — it
        // matches a sibling name, so the keep-filter retains it and resolution
        // anchors it to the current module.
        assert!(refs.contains(&"demo::helper".to_string()), "got {refs:?}");
    }

    #[test]
    fn code_path_skips_own_declaration_ident() {
        // A never-used item's own declaring ident must NOT be recorded as a
        // reference to itself — otherwise `unused-pub` sees a same-crate ref and
        // misclassifies it `IntraCrate` ("pub(crate)") instead of `Unused`.
        let refs = collect_refs("fn lonely() { let _ = 1; }", "demo");
        assert!(
            !refs.contains(&"demo::lonely".to_string()),
            "declaration self-ref recorded; got {refs:?}"
        );
    }

    #[test]
    fn code_path_keeps_recursive_self_call() {
        // The skip is span-based, not name-based: a recursive *call* in the body
        // sits at a different span than the declaration, so it stays a genuine
        // reference (only the declaring ident itself is dropped).
        let refs = collect_refs("fn recur() { recur(); }", "demo");
        assert!(refs.contains(&"demo::recur".to_string()), "got {refs:?}");
    }

    #[test]
    fn code_path_resolves_function_local_module_use() {
        // A `use crate::m::sub;` *inside a fn body*, then `sub::ITEM`: the
        // crate-local const must be seen as referenced (regex-syntax's
        // `age::BY_NAME` FP class — module imported in a fn, member accessed by
        // `Mod::ITEM`). Module-level uses alone wouldn't catch this.
        let refs = collect_refs(
            "mod m { pub mod sub { pub const ITEM: u32 = 0; } } \
             fn f() { use crate::m::sub; let _ = sub::ITEM; }",
            "demo",
        );
        assert!(
            refs.contains(&"demo::m::sub::ITEM".to_string()),
            "function-local module-import use not honored; got {refs:?}"
        );
    }

    #[test]
    fn code_path_resolves_function_local_braced_use() {
        // A function-local braced `use crate::m::{sub::ITEM, other};`, then a
        // bare `ITEM` (regex-automata's `PERL_WORD` FP class — braced group with
        // a shared `crate::m` prefix imported inside a fn).
        let refs = collect_refs(
            "mod m { pub mod sub { pub const ITEM: u32 = 0; } pub fn other() {} } \
             fn f() { use crate::m::{sub::ITEM, other}; let _ = ITEM; other(); }",
            "demo",
        );
        assert!(
            refs.contains(&"demo::m::sub::ITEM".to_string()),
            "function-local braced use not honored; got {refs:?}"
        );
    }

    #[test]
    fn code_path_records_bare_sibling_type_reference() {
        // The thiserror FP class: a sibling type referenced by *bare* name in a
        // field type (`Option<Sib>`) and a struct literal (`Sib { .. }`) must be
        // recorded, so `unused-pub` doesn't think `Sib` is unused.
        let refs = collect_refs(
            "struct Sib; struct Wrap { inner: Option<Sib> } fn mk() -> Sib { Sib }",
            "demo",
        );
        assert!(
            refs.contains(&"demo::Sib".to_string()),
            "bare sibling type ref not recorded; got {refs:?}"
        );
    }

    #[test]
    fn code_path_skips_unmatched_single_ident() {
        let refs = collect_refs("fn f() { let x = 5; let _ = x; }", "demo");
        assert!(
            !refs.iter().any(|r| r == "x"),
            "got {refs:?} — bare locals should not be recorded as references"
        );
    }

    #[test]
    fn code_path_captures_extern_crate() {
        let refs = collect_refs("extern crate foo;", "demo");
        assert!(refs.contains(&"foo".to_string()), "got {refs:?}");
    }

    #[test]
    fn code_path_captures_macro_invocation_path() {
        let refs = collect_refs("fn f() { serde_json::json!({\"a\": 1}); }", "demo");
        assert!(
            refs.contains(&"serde_json::json".to_string()),
            "got {refs:?}"
        );
    }

    #[test]
    fn code_path_skips_use_statements() {
        // Use statements produce use_bindings, not references.
        let refs = collect_refs("use foo::Bar;", "demo");
        // We expect no entries for `foo::Bar` in references — that's a binding.
        assert!(
            !refs.contains(&"foo::Bar".to_string()),
            "got {refs:?} — use statements should not contribute to references"
        );
    }

    #[test]
    fn code_path_skips_macro_rules_definitions() {
        // macro_rules! bodies feed macro_implicit_refs, not references.
        let src = "macro_rules! m { () => { foo::bar() }; }";
        let refs = collect_refs(src, "demo");
        assert!(
            !refs.contains(&"foo::bar".to_string()),
            "got {refs:?} — macro_rules bodies belong in macro_implicit_refs"
        );
    }

    #[test]
    fn code_path_captures_struct_field_types() {
        let refs = collect_refs("use other::Inner; struct S { f: Inner }", "demo");
        assert!(refs.contains(&"other::Inner".to_string()), "got {refs:?}");
    }
}
