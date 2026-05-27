//! Tier 1: per-file `use` and `use ... as ...` tracking.
//!
//! Walks a `syn::ItemUse` and emits one [`UseBinding`] per imported name,
//! with each binding's canonical path resolved relative to the importing
//! file's [`Scope`]:
//!
//! - `crate::` and `self::` are anchored to the importing crate's root or
//!   current module respectively.
//! - One or more leading `super::` segments climb the module tree.
//! - Leading-`::` (absolute) and unqualified paths (`use serde::Deserialize`)
//!   are treated as external-or-workspace crate names at the leading segment.
//! - `use foo::*;` (glob) is intentionally **not** expanded here — tier 1
//!   produces no binding for globs. Tier 2 expands globs that target
//!   workspace crates; external-crate globs land in `known_false_negatives`
//!   because we can't enumerate external exports without rustdoc JSON.
//!
//! Renames (`use foo::Bar as Baz`) record both the local binding (`Baz`) and
//! the canonical path at the definition site (`foo::Bar`). Downstream lints
//! query the canonical path; rename loss would produce false-positive
//! "unused" reports.

use super::{ResolvedPath, Visibility};

/// One rename entry produced by walking a `use` declaration.
///
/// `local_name` is what the name binds to in the importing scope (the LHS of
/// a `Rename` use-tree, or the trailing segment of a `Path`/`Name`).
/// `canonical` is the fully-qualified path the name refers to at the
/// definition site. `visibility` reflects the `use` declaration's own
/// visibility — `pub use` produces re-export edges followed by Tier 2.5.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UseBinding {
    pub local_name: String,
    pub canonical: ResolvedPath,
    pub visibility: Visibility,
}

/// Where in the workspace a file lives, for resolving `crate`/`self`/`super`
/// path prefixes.
///
/// `module_path` is the chain of `mod` names from the crate root to the
/// importing file, **not including the crate name**. A file at the crate
/// root has `module_path: []`; a file at `crates/demo/src/a/b.rs` (reached
/// via `mod a; mod a::b;`) has `module_path: ["a", "b"]`.
#[derive(Debug, Clone)]
pub struct Scope {
    pub crate_name: String,
    pub module_path: Vec<String>,
}

impl Scope {
    pub fn new(crate_name: impl Into<String>) -> Self {
        Self {
            crate_name: crate_name.into(),
            module_path: Vec::new(),
        }
    }

    pub fn with_module<I, S>(mut self, segments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.module_path
            .extend(segments.into_iter().map(Into::into));
        self
    }
}

/// Walk a `use` declaration and emit one [`UseBinding`] per leaf name.
///
/// All bindings inherit the visibility of the `use` declaration itself —
/// `pub use foo::{Bar, Baz};` produces two bindings, both `Public`.
pub fn bindings_from_use(item: &syn::ItemUse, scope: &Scope) -> Vec<UseBinding> {
    let mut prefix: Vec<String> = Vec::new();
    let mut tree: &syn::UseTree = &item.tree;

    if item.leading_colon.is_none() {
        peel_leading_special(&mut tree, &mut prefix, scope);
    }

    let visibility = Visibility::from_syn(&item.vis);
    let mut out = Vec::new();
    walk(tree, &prefix, visibility, &mut out);
    out
}

/// Strip leading `crate::` / `self::` / `super::` segments from `tree`,
/// updating `prefix` to encode their effect. After this call, `tree` points
/// to the first non-special path segment (or the original tree if the use
/// didn't start with a special segment).
fn peel_leading_special(tree: &mut &syn::UseTree, prefix: &mut Vec<String>, scope: &Scope) {
    let mut started = false;
    while let syn::UseTree::Path(path) = tree {
        let ident = path.ident.to_string();
        let consumed = match ident.as_str() {
            "crate" if !started => {
                prefix.clear();
                prefix.push(scope.crate_name.clone());
                started = true;
                true
            }
            "self" if !started => {
                prefix.clear();
                prefix.push(scope.crate_name.clone());
                prefix.extend(scope.module_path.iter().cloned());
                started = true;
                true
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
                true
            }
            _ => false,
        };
        if consumed {
            *tree = &path.tree;
        } else {
            break;
        }
    }
}

fn walk(tree: &syn::UseTree, prefix: &[String], visibility: Visibility, out: &mut Vec<UseBinding>) {
    match tree {
        syn::UseTree::Path(p) => {
            let mut new_prefix = prefix.to_vec();
            new_prefix.push(p.ident.to_string());
            walk(&p.tree, &new_prefix, visibility, out);
        }
        syn::UseTree::Name(n) => {
            let name = n.ident.to_string();
            let mut canon = prefix.to_vec();
            canon.push(name.clone());
            out.push(UseBinding {
                local_name: name,
                canonical: ResolvedPath::new(canon),
                visibility,
            });
        }
        syn::UseTree::Rename(r) => {
            let mut canon = prefix.to_vec();
            canon.push(r.ident.to_string());
            out.push(UseBinding {
                local_name: r.rename.to_string(),
                canonical: ResolvedPath::new(canon),
                visibility,
            });
        }
        syn::UseTree::Glob(_) => {
            // Tier 1 emits no binding for globs. Tier 2 expands globs that
            // target workspace crates; external-crate globs land in
            // `known_false_negatives` because we can't enumerate exports
            // without rustdoc JSON.
        }
        syn::UseTree::Group(g) => {
            for item in &g.items {
                walk(item, prefix, visibility, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> syn::ItemUse {
        syn::parse_str(src).expect("valid use item")
    }

    fn scope(crate_name: &str, modules: &[&str]) -> Scope {
        Scope::new(crate_name).with_module(modules.iter().copied())
    }

    fn bindings(src: &str, scope: &Scope) -> Vec<(String, String)> {
        bindings_from_use(&parse(src), scope)
            .into_iter()
            .map(|b| (b.local_name, b.canonical.display()))
            .collect()
    }

    #[test]
    fn external_crate_terminal_name() {
        let s = scope("demo", &[]);
        assert_eq!(
            bindings("use std::fmt::Display;", &s),
            vec![("Display".into(), "std::fmt::Display".into())]
        );
    }

    #[test]
    fn rename_records_both_local_and_canonical() {
        let s = scope("demo", &[]);
        assert_eq!(
            bindings("use foo::Bar as Baz;", &s),
            vec![("Baz".into(), "foo::Bar".into())]
        );
    }

    #[test]
    fn crate_prefix_anchors_to_current_crate() {
        let s = scope("demo", &["other"]);
        assert_eq!(
            bindings("use crate::foo::Bar;", &s),
            vec![("Bar".into(), "demo::foo::Bar".into())]
        );
    }

    #[test]
    fn self_prefix_anchors_to_current_module() {
        let s = scope("demo", &["sub"]);
        assert_eq!(
            bindings("use self::Foo;", &s),
            vec![("Foo".into(), "demo::sub::Foo".into())]
        );
    }

    #[test]
    fn super_climbs_one_module() {
        let s = scope("demo", &["a", "b"]);
        assert_eq!(
            bindings("use super::Foo;", &s),
            vec![("Foo".into(), "demo::a::Foo".into())]
        );
    }

    #[test]
    fn super_super_climbs_two_modules() {
        let s = scope("demo", &["a", "b", "c"]);
        assert_eq!(
            bindings("use super::super::Foo;", &s),
            vec![("Foo".into(), "demo::a::Foo".into())]
        );
    }

    #[test]
    fn super_at_crate_root_stops_at_crate_name() {
        // `use super::Foo;` from a file at the crate root is invalid Rust,
        // but the resolver shouldn't panic — it should saturate at the
        // crate name. The downstream lint will catch the invalid file.
        let s = scope("demo", &[]);
        assert_eq!(
            bindings("use super::Foo;", &s),
            vec![("Foo".into(), "demo::Foo".into())]
        );
    }

    #[test]
    fn group_expands_to_one_binding_per_leaf() {
        let s = scope("demo", &[]);
        let mut got = bindings("use foo::{Bar, Baz};", &s);
        got.sort();
        assert_eq!(
            got,
            vec![
                ("Bar".into(), "foo::Bar".into()),
                ("Baz".into(), "foo::Baz".into()),
            ]
        );
    }

    #[test]
    fn nested_group_with_rename_inside_crate() {
        let s = scope("demo", &[]);
        let mut got = bindings("use crate::{a::B, c::{D, E as F}};", &s);
        got.sort();
        assert_eq!(
            got,
            vec![
                ("B".into(), "demo::a::B".into()),
                ("D".into(), "demo::c::D".into()),
                ("F".into(), "demo::c::E".into()),
            ]
        );
    }

    #[test]
    fn glob_imports_emit_no_binding_at_tier_1() {
        let s = scope("demo", &[]);
        assert!(bindings("use foo::*;", &s).is_empty());
    }

    #[test]
    fn leading_colon_absolute_path_drops_special_handling() {
        let s = scope("demo", &["sub"]);
        // ::serde::Deserialize is absolute — `crate`/`self`/`super` would
        // not be valid here, so the peel step is skipped.
        assert_eq!(
            bindings("use ::serde::Deserialize;", &s),
            vec![("Deserialize".into(), "serde::Deserialize".into())]
        );
    }

    #[test]
    fn group_at_root_treats_each_item_as_a_crate_name() {
        let s = scope("demo", &[]);
        let mut got = bindings("use {std, core};", &s);
        got.sort();
        assert_eq!(
            got,
            vec![("core".into(), "core".into()), ("std".into(), "std".into())]
        );
    }

    #[test]
    fn terminal_path_with_one_segment_external() {
        // `use std;` binds the name `std` to the std crate.
        let s = scope("demo", &[]);
        assert_eq!(bindings("use std;", &s), vec![("std".into(), "std".into())]);
    }

    #[test]
    fn private_use_carries_private_visibility() {
        let s = scope("demo", &[]);
        let got = bindings_from_use(&parse("use foo::Bar;"), &s);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].visibility, Visibility::Private);
    }

    #[test]
    fn pub_use_carries_public_visibility() {
        let s = scope("demo", &[]);
        let got = bindings_from_use(&parse("pub use foo::Bar;"), &s);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].visibility, Visibility::Public);
    }

    #[test]
    fn pub_crate_use_carries_pub_crate_visibility() {
        let s = scope("demo", &[]);
        let got = bindings_from_use(&parse("pub(crate) use foo::Bar;"), &s);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].visibility, Visibility::PubCrate);
    }

    #[test]
    fn group_inherits_use_declaration_visibility() {
        let s = scope("demo", &[]);
        let got = bindings_from_use(&parse("pub use foo::{Bar, Baz};"), &s);
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|b| b.visibility == Visibility::Public));
    }
}
