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
    BrokenModDecl, Error, Item, ItemKind, Module, ResolvedPath, Result, SourceSpan, Visibility,
};

/// Items, submodules, `use` bindings, broken `mod` declarations,
/// `#[cfg(feature = "...")]` references, and `macro_rules!`-body implicit
/// references collected while walking a module.
struct ModuleContents {
    items: Vec<Item>,
    submodules: Vec<Module>,
    use_bindings: Vec<UseBinding>,
    broken_mod_decls: Vec<BrokenModDecl>,
    cfg_features: Vec<String>,
    macro_implicit_refs: Vec<ResolvedPath>,
}

/// Build a fully-populated module tree for one crate.
///
/// Returns an empty placeholder [`Module`] if the crate has neither `lib.rs`
/// nor `main.rs` at the standard location — for non-standard layouts, the
/// caller should pass an explicit entry point to a future variant.
pub fn build_crate_tree(manifest_dir: &Path, crate_name: &str) -> Result<Module> {
    let src_dir = manifest_dir.join("src");
    let candidates = [src_dir.join("lib.rs"), src_dir.join("main.rs")];

    let Some(root_file) = candidates.iter().find(|p| p.exists()) else {
        return Ok(empty_root(crate_name));
    };

    let crate_root_path = ResolvedPath::new([crate_name.to_string()]);
    build_module_from_file(root_file, crate_name.to_string(), crate_root_path)
}

fn empty_root(crate_name: &str) -> Module {
    Module {
        name: crate_name.to_string(),
        canonical: ResolvedPath::new([crate_name.to_string()]),
        items: Vec::new(),
        submodules: Vec::new(),
        use_bindings: Vec::new(),
        broken_mod_decls: Vec::new(),
        cfg_features: Vec::new(),
        macro_implicit_refs: Vec::new(),
        file: None,
    }
}

fn build_module_from_file(
    file_path: &Path,
    mod_name: String,
    canonical: ResolvedPath,
) -> Result<Module> {
    let source = std::fs::read_to_string(file_path)?;
    let parsed = syn::parse_file(&source).map_err(|e| Error::Parse {
        path: file_path.to_path_buf(),
        message: e.to_string(),
    })?;

    let contents = collect_module_contents(&parsed.items, file_path, &canonical)?;

    Ok(Module {
        name: mod_name,
        canonical,
        items: contents.items,
        submodules: contents.submodules,
        use_bindings: contents.use_bindings,
        broken_mod_decls: contents.broken_mod_decls,
        cfg_features: contents.cfg_features,
        macro_implicit_refs: contents.macro_implicit_refs,
        file: Some(file_path.to_path_buf()),
    })
}

fn collect_module_contents(
    syn_items: &[syn::Item],
    parent_file: &Path,
    parent_canonical: &ResolvedPath,
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
    let mut macro_refs: std::collections::BTreeSet<ResolvedPath> =
        std::collections::BTreeSet::new();

    for syn_item in syn_items {
        for attr in item_attrs(syn_item) {
            extract_cfg_feature_names(attr, &mut cfg_features);
        }

        if let syn::Item::Use(item_use) = syn_item {
            let mut bindings = use_tree::bindings_from_use(item_use, &scope);
            for binding in &mut bindings {
                rewrite_sibling_local(binding, parent_canonical, &sibling_names);
            }
            use_bindings.extend(bindings);
        }

        if let syn::Item::Macro(item_macro) = syn_item
            && item_macro.ident.is_some()
        {
            // `macro_rules!` definition — scan its body for path-like
            // token sequences and resolve through this scope.
            extract_macro_paths(
                item_macro.mac.tokens.clone(),
                &scope,
                &sibling_names,
                parent_canonical,
                &mut macro_refs,
            );
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
                let inline = collect_module_contents(inline_items, parent_file, &child_canonical)?;
                submodules.push(Module {
                    name: child_name,
                    canonical: child_canonical,
                    items: inline.items,
                    submodules: inline.submodules,
                    use_bindings: inline.use_bindings,
                    broken_mod_decls: inline.broken_mod_decls,
                    cfg_features: inline.cfg_features,
                    macro_implicit_refs: inline.macro_implicit_refs,
                    file: Some(parent_file.to_path_buf()),
                });
            } else if let Some(child_file) = resolve_mod_file(parent_file, item_mod)? {
                submodules.push(build_module_from_file(
                    &child_file,
                    child_name,
                    child_canonical,
                )?);
            } else {
                // `mod foo;` with neither inline body nor backing file —
                // record so the module-tree lint can flag it.
                broken_mod_decls.push(BrokenModDecl {
                    name: child_name,
                    declared_in: parent_file.to_path_buf(),
                    line: item_mod.mod_token.span.start().line as u32,
                });
            }
        }
    }

    Ok(ModuleContents {
        items,
        submodules,
        use_bindings,
        broken_mod_decls,
        cfg_features: cfg_features.into_iter().collect(),
        macro_implicit_refs: macro_refs.into_iter().collect(),
    })
}

/// Scan a `macro_rules!` body token-stream for path-like sequences
/// (`Ident :: Ident (:: Ident)*`) and resolve each through the macro's
/// defining scope. Records the resolved path in `out`.
///
/// This is intentionally conservative: any multi-segment path that *looks*
/// like a reference becomes one. Single identifiers (parameter names,
/// keywords, etc.) are dropped. Token groups are recursed into so paths
/// inside nested braces/parens/brackets are still seen.
fn extract_macro_paths(
    tokens: proc_macro2::TokenStream,
    scope: &use_tree::Scope,
    siblings: &HashSet<String>,
    parent_canonical: &ResolvedPath,
    out: &mut std::collections::BTreeSet<ResolvedPath>,
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
            if segments.len() >= 2
                && let Some(resolved) =
                    resolve_macro_path(segments, scope, siblings, parent_canonical)
            {
                out.insert(resolved);
            }
            i = j;
            continue;
        }
        if let proc_macro2::TokenTree::Group(group) = &stream[i] {
            extract_macro_paths(group.stream(), scope, siblings, parent_canonical, out);
        }
        i += 1;
    }
}

/// Apply the same crate/self/super peeling and sibling-prepend logic that
/// `bindings_from_use` does, but for a path extracted from raw tokens.
fn resolve_macro_path(
    segments: Vec<String>,
    scope: &use_tree::Scope,
    siblings: &HashSet<String>,
    parent_canonical: &ResolvedPath,
) -> Option<ResolvedPath> {
    if segments.is_empty() {
        return None;
    }
    let mut iter = segments.into_iter().peekable();
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
                if prefix.len() > 1 {
                    prefix.pop();
                }
                iter.next();
            }
            _ => break,
        }
    }
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

fn item_from_syn(item: &syn::Item, parent_canonical: &ResolvedPath, file: &Path) -> Option<Item> {
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
                }),
            });
        }
        _ => return None,
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
        }),
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
        // BrokenModDecl entry on the parent module so the module-tree lint
        // can flag it.
        let root = build_crate_tree(&manifest_dir("missing_mod"), "missing_mod").expect("build");
        assert!(root.submodules.iter().all(|m| m.name != "ghost"));
        assert!(
            root.broken_mod_decls.iter().any(|d| d.name == "ghost"),
            "expected `ghost` to be recorded as a broken mod decl, got: {:?}",
            root.broken_mod_decls,
        );
    }
}
