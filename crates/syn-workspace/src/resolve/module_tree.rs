//! Tier 2: cross-file module tree assembly.
//!
//! For each crate, starts at `<manifest_dir>/src/lib.rs` (preferred) or
//! `<manifest_dir>/src/main.rs` and walks every `mod foo;` declaration to:
//!
//! - `foo.rs` adjacent to the parent file, or
//! - `foo/mod.rs` in a subdirectory, or
//! - the file named by a `#[path = "..."]` attribute override (relative to
//!   the parent file's directory).
//!
//! Produces a tree of [`Module`] values rooted at the crate root, each
//! populated with the items declared at that scope. Inline `mod foo { ... }`
//! blocks become submodules backed by the same `file` as their parent.
//!
//! Edge cases tracked as `known_false_*` until handled:
//! - `#[cfg_attr(cond, path = "...")]` (we don't evaluate cfg-attr expansion)
//! - `include!("…")` (we don't follow include directives)
//! - Multi-target crates (libraries + binaries + examples) — currently only
//!   the primary library or binary root is loaded.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::use_tree::{self, UseBinding};
use super::{
    BrokenModDecl, Error, Item, ItemKind, Module, Occurrence, Origin, ResolvedPath, Result,
    SourceSpan, Visibility,
};
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
    build_module_from_file(
        root_file,
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
        file: None,
    }
}

pub(crate) fn build_module_from_file(
    file_path: &Path,
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

    let contents = collect_module_contents(&parsed.items, file_path, &canonical, marker_crates)?;

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
        file: Some(file_path.to_path_buf()),
    })
}

fn collect_module_contents(
    syn_items: &[syn::Item],
    parent_file: &Path,
    parent_canonical: &ResolvedPath,
    marker_crates: &[String],
) -> Result<ModuleContents> {
    let scope = scope_from(parent_canonical);
    // Names declared at this module level. A `use foo::Bar;` whose first
    // segment matches one of these refers to a crate-local sibling, not an
    // external crate — see Rust 2018+ path resolution rules. Order in source
    // doesn't matter, so we collect names in one pass before processing use
    // statements.
    let sibling_names: HashSet<String> = syn_items.iter().filter_map(sibling_name).collect();

    let mut items = Vec::new();
    let mut submodules = Vec::new();
    let mut use_bindings = Vec::new();
    let mut broken_mod_decls = Vec::new();
    let mut cfg_features: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    // All reference occurrences for this module (macro-body + regular-code +
    // glob + extern-crate). Phase B (below) resolves each in place; this Vec is
    // the module's reference surface, stored directly on `Module.occurrences`.
    let mut occurrences: Vec<Occurrence> = Vec::new();

    for syn_item in syn_items {
        for attr in item_attrs(syn_item) {
            extract_cfg_feature_names(attr, &mut cfg_features);
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
            let lowerers = plugins::builtin_lowerers();
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
                let inline = collect_module_contents(
                    inline_items,
                    parent_file,
                    &child_canonical,
                    marker_crates,
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
                    file: Some(parent_file.to_path_buf()),
                });
            } else if let Some(child_file) = resolve_mod_file(parent_file, item_mod)? {
                submodules.push(build_module_from_file(
                    &child_file,
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
                for target in use_tree::glob_targets_from_use(item_use, &scope) {
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

        let tokens = quote::ToTokens::to_token_stream(syn_item);
        extract_code_paths(tokens, &use_bindings, parent_file, &mut occurrences);
    }

    // Phase B: resolve every raw occurrence centrally, filling in its canonical
    // `path` in place (occurrences that don't resolve keep `path = None`). The
    // resolved occurrences are this module's reference surface.
    for occ in &mut occurrences {
        let resolved =
            resolve_occurrence(occ, &scope, &use_bindings, &sibling_names, parent_canonical);
        occ.path = resolved;
    }

    Ok(ModuleContents {
        items,
        submodules,
        use_bindings,
        broken_mod_decls,
        cfg_features: cfg_features.into_iter().collect(),
        occurrences,
    })
}

/// Candidate-select path references from a regular (non-macro) item body: scan
/// for `Ident :: Ident (:: Ident)*` runs and emit each as a raw `Origin::Code`
/// [`Occurrence`] (segments + span). Resolution — crate/self/super peeling,
/// use-binding substitution, sibling rewrite — happens later and centrally in
/// [`resolve_occurrence`].
///
/// The only resolution-aware decision kept here is candidate SELECTION: a bare
/// single ident is emitted only if it matches a `use`-binding's `local_name`
/// (otherwise it's a local/prelude name); multi-segment runs are always
/// emitted. `use_bindings` is passed solely for that keep-filter.
fn extract_code_paths(
    tokens: proc_macro2::TokenStream,
    use_bindings: &[UseBinding],
    file: &Path,
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
            // Candidate selection only — resolution happens centrally in
            // `resolve_occurrence`. Keep multi-segment runs, plus single idents
            // that match a use-binding's local name (the binding set is needed
            // here, but the substitution itself is deferred to resolution).
            let keep = segments.len() >= 2
                || (segments.len() == 1
                    && use_bindings.iter().any(|b| b.local_name == segments[0]));
            if keep {
                out.push(Occurrence {
                    segments,
                    path: None,
                    span: Some(span_to_source_span(file, first.span())),
                    origin: Origin::Code,
                });
            }
            i = j;
            continue;
        }
        if let proc_macro2::TokenTree::Group(group) = &stream[i] {
            extract_code_paths(group.stream(), use_bindings, file, out);
        }
        i += 1;
    }
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

/// Names declared at a module's lexical scope — function/struct/enum/etc.
/// idents plus child module names. Used to distinguish "crate-local sibling"
/// from "external crate" at the leading segment of a `use` path.
fn sibling_name(item: &syn::Item) -> Option<String> {
    match item {
        syn::Item::Fn(i) => Some(i.sig.ident.to_string()),
        syn::Item::Struct(i) => Some(i.ident.to_string()),
        syn::Item::Enum(i) => Some(i.ident.to_string()),
        syn::Item::Union(i) => Some(i.ident.to_string()),
        syn::Item::Trait(i) => Some(i.ident.to_string()),
        syn::Item::Type(i) => Some(i.ident.to_string()),
        syn::Item::Const(i) => Some(i.ident.to_string()),
        syn::Item::Static(i) => Some(i.ident.to_string()),
        syn::Item::Mod(i) => Some(i.ident.to_string()),
        syn::Item::Macro(i) => i.ident.as_ref().map(ToString::to_string),
        _ => None,
    }
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

/// Locate the source file backing a `mod foo;` declaration.
///
/// Honors `#[path = "..."]` overrides (relative to the parent file's
/// directory). Falls back to `<dir>/foo.rs` then `<dir>/foo/mod.rs`.
fn resolve_mod_file(parent_file: &Path, item_mod: &syn::ItemMod) -> Result<Option<PathBuf>> {
    let parent_dir = parent_file.parent().unwrap_or(Path::new("."));
    let mod_name = item_mod.ident.to_string();

    if let Some(override_path) = path_attribute(&item_mod.attrs) {
        let candidate = parent_dir.join(&override_path);
        return Ok(candidate.exists().then_some(candidate));
    }

    let adjacent = parent_dir.join(format!("{mod_name}.rs"));
    if adjacent.exists() {
        return Ok(Some(adjacent));
    }

    let nested = parent_dir.join(&mod_name).join("mod.rs");
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
            &parent_canonical,
            &markers,
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
        // `helper` matches a sibling; first segment of `helper::Sub::go`
        // resolves crate-local (note: `helper` alone is a single-ident sibling
        // call — those are NOT recorded since they don't survive the
        // single-ident filter without a use-binding).
        assert!(
            refs.contains(&"demo::helper::Sub::go".to_string()),
            "got {refs:?}"
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
