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

use std::path::{Path, PathBuf};

use super::use_tree::{self, UseBinding};
use super::{Error, Item, ItemKind, Module, ResolvedPath, Result, SourceSpan, Visibility};

/// Items, submodules, and `use` bindings collected while walking a module.
struct ModuleContents {
    items: Vec<Item>,
    submodules: Vec<Module>,
    use_bindings: Vec<UseBinding>,
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
        file: Some(file_path.to_path_buf()),
    })
}

fn collect_module_contents(
    syn_items: &[syn::Item],
    parent_file: &Path,
    parent_canonical: &ResolvedPath,
) -> Result<ModuleContents> {
    let scope = scope_from(parent_canonical);
    let mut items = Vec::new();
    let mut submodules = Vec::new();
    let mut use_bindings = Vec::new();

    for syn_item in syn_items {
        if let syn::Item::Use(item_use) = syn_item {
            use_bindings.extend(use_tree::bindings_from_use(item_use, &scope));
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
                    file: Some(parent_file.to_path_buf()),
                });
            } else if let Some(child_file) = resolve_mod_file(parent_file, item_mod)? {
                submodules.push(build_module_from_file(
                    &child_file,
                    child_name,
                    child_canonical,
                )?);
            }
            // If the declaration has no body and no resolvable file, the
            // `module-tree` lint's `mod_decl_missing_target` case will fire.
            // We silently drop here so the workspace model stays consistent.
        }
    }

    Ok(ModuleContents {
        items,
        submodules,
        use_bindings,
    })
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
        visibility: vis_from_syn(vis),
        canonical: ResolvedPath::new(canonical),
        source: Some(SourceSpan {
            file: file.to_path_buf(),
            line: line as u32,
            column: 1,
        }),
    })
}

fn vis_from_syn(v: &syn::Visibility) -> Visibility {
    match v {
        syn::Visibility::Public(_) => Visibility::Public,
        syn::Visibility::Restricted(r) => {
            if r.path.is_ident("crate") {
                Visibility::PubCrate
            } else if r.path.is_ident("super") {
                Visibility::PubSuper
            } else {
                Visibility::PubIn
            }
        }
        syn::Visibility::Inherited => Visibility::Private,
    }
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
    fn missing_mod_target_is_silently_skipped() {
        // `mod ghost;` with no file should not panic; the module-tree lint
        // emits a separate `mod_decl_missing_target` diagnostic.
        let root = build_crate_tree(&manifest_dir("missing_mod"), "missing_mod").expect("build");
        assert!(root.submodules.iter().all(|m| m.name != "ghost"));
    }
}
