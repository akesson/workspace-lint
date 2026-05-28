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
//! the canonical path at the definition site (`foo::Bar`). Consumers query
//! the canonical path; rename loss would silently turn "imported and used"
//! into "unknown reference."

use std::path::Path;

use super::{ResolvedPath, SourceSpan, Visibility};

/// One rename entry produced by walking a `use` declaration.
///
/// `local_name` is what the name binds to in the importing scope (the LHS of
/// a `Rename` use-tree, or the trailing segment of a `Path`/`Name`).
/// `canonical` is the fully-qualified path the name refers to at the
/// definition site. `visibility` reflects the `use` declaration's own
/// visibility — `pub use` produces re-export edges followed by Tier 2.5.
///
/// `source` carries the location of the leaf ident that produced this
/// binding (the imported / renamed name itself). For a group like
/// `use foo::{Bar, Baz};` the two bindings get distinct spans, so
/// downstream lints can point at the specific offending leaf. `None`
/// for bindings synthesized outside the parser (test helpers, mocks).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UseBinding {
    pub local_name: String,
    pub canonical: ResolvedPath,
    pub visibility: Visibility,
    pub source: Option<SourceSpan>,
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
///
/// `file` is the path of the source file the `use` was parsed from; it's
/// recorded on each binding's [`UseBinding::source`] so downstream lints
/// can emit line-accurate diagnostics.
pub fn bindings_from_use(item: &syn::ItemUse, scope: &Scope, file: &Path) -> Vec<UseBinding> {
    let mut prefix: Vec<String> = Vec::new();
    let mut tree: &syn::UseTree = &item.tree;

    if item.leading_colon.is_none() {
        peel_leading_special(&mut tree, &mut prefix, scope);
    }

    let visibility = Visibility::from_syn(&item.vis);
    let mut out = Vec::new();
    walk(tree, &prefix, visibility, file, &mut out);
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

fn walk(
    tree: &syn::UseTree,
    prefix: &[String],
    visibility: Visibility,
    file: &Path,
    out: &mut Vec<UseBinding>,
) {
    match tree {
        syn::UseTree::Path(p) => {
            let mut new_prefix = prefix.to_vec();
            new_prefix.push(p.ident.to_string());
            walk(&p.tree, &new_prefix, visibility, file, out);
        }
        syn::UseTree::Name(n) => {
            let name = n.ident.to_string();
            let mut canon = prefix.to_vec();
            canon.push(name.clone());
            out.push(UseBinding {
                local_name: name,
                canonical: ResolvedPath::new(canon),
                visibility,
                source: Some(source_span_from_ident(file, &n.ident)),
            });
        }
        syn::UseTree::Rename(r) => {
            let mut canon = prefix.to_vec();
            canon.push(r.ident.to_string());
            out.push(UseBinding {
                local_name: r.rename.to_string(),
                canonical: ResolvedPath::new(canon),
                visibility,
                // Anchor at the canonical (LHS) ident — that's what the
                // binding *resolves to*, and what downstream lints will
                // most often want to flag.
                source: Some(source_span_from_ident(file, &r.ident)),
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
                walk(item, prefix, visibility, file, out);
            }
        }
    }
}

/// Convert a `syn::Ident`'s span into a [`SourceSpan`] anchored at `file`.
/// The `byte_range` helper lives in `module_tree.rs`; we re-use it so the
/// "synthetic span → `None`" sentinel logic stays in one place.
fn source_span_from_ident(file: &Path, ident: &proc_macro2::Ident) -> SourceSpan {
    let start = ident.span().start();
    SourceSpan {
        file: file.to_path_buf(),
        line: start.line as u32,
        column: start.column as u32,
        byte_range: super::module_tree::byte_range(ident.span()),
    }
}

/// Collect the canonical prefix of every glob (`use foo::bar::*;`) in the
/// given `use` item. Tier 1's [`bindings_from_use`] deliberately skips globs
/// because there's no specific local name to bind; this companion walker
/// exists so the references pass can still record what the glob targeted.
/// Without it, `use predicates::prelude::*;` would look like a no-op and
/// any dep-usage analysis would wrongly conclude `predicates` is unused.
pub fn glob_targets_from_use(item: &syn::ItemUse, scope: &Scope) -> Vec<ResolvedPath> {
    let mut prefix: Vec<String> = Vec::new();
    let mut tree: &syn::UseTree = &item.tree;
    if item.leading_colon.is_none() {
        peel_leading_special(&mut tree, &mut prefix, scope);
    }
    let mut out = Vec::new();
    walk_globs(tree, &prefix, &mut out);
    out
}

fn walk_globs(tree: &syn::UseTree, prefix: &[String], out: &mut Vec<ResolvedPath>) {
    match tree {
        syn::UseTree::Path(p) => {
            let mut new_prefix = prefix.to_vec();
            new_prefix.push(p.ident.to_string());
            walk_globs(&p.tree, &new_prefix, out);
        }
        syn::UseTree::Glob(_) => {
            if !prefix.is_empty() {
                out.push(ResolvedPath::new(prefix.to_vec()));
            }
        }
        syn::UseTree::Group(g) => {
            for item in &g.items {
                walk_globs(item, prefix, out);
            }
        }
        syn::UseTree::Name(_) | syn::UseTree::Rename(_) => {
            // Non-glob leaves are handled by bindings_from_use.
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

    /// Stand-in path for unit tests — `bindings_from_use` records this as
    /// the source file for every binding it emits. Tests that compare
    /// local name + canonical only just discard it; tests that assert
    /// against the source span use it as a sentinel.
    const FAKE_FILE: &str = "tests/fixture.rs";

    fn bindings(src: &str, scope: &Scope) -> Vec<(String, String)> {
        bindings_from_use(&parse(src), scope, Path::new(FAKE_FILE))
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
        // crate name. The compiler will catch the invalid file at build
        // time, not us.
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
        let got = bindings_from_use(&parse("use foo::Bar;"), &s, Path::new(FAKE_FILE));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].visibility, Visibility::Private);
    }

    #[test]
    fn pub_use_carries_public_visibility() {
        let s = scope("demo", &[]);
        let got = bindings_from_use(&parse("pub use foo::Bar;"), &s, Path::new(FAKE_FILE));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].visibility, Visibility::Public);
    }

    #[test]
    fn pub_crate_use_carries_pub_crate_visibility() {
        let s = scope("demo", &[]);
        let got = bindings_from_use(&parse("pub(crate) use foo::Bar;"), &s, Path::new(FAKE_FILE));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].visibility, Visibility::PubCrate);
    }

    #[test]
    fn group_inherits_use_declaration_visibility() {
        let s = scope("demo", &[]);
        let got = bindings_from_use(&parse("pub use foo::{Bar, Baz};"), &s, Path::new(FAKE_FILE));
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|b| b.visibility == Visibility::Public));
    }

    // --- source spans ---

    #[test]
    fn source_records_file_and_line_for_each_leaf() {
        let s = scope("demo", &[]);
        let got = bindings_from_use(&parse("use foo::Bar;"), &s, Path::new(FAKE_FILE));
        let span = got[0].source.as_ref().expect("source populated");
        assert_eq!(span.file, Path::new(FAKE_FILE));
        // `proc_macro2` reports the ident on the first (only) line of a
        // single-line parse; that's enough to prove the line plumbing
        // is wired. Byte range is `Some` for synthesized parses too,
        // since they get real byte offsets.
        assert_eq!(span.line, 1);
        assert!(span.byte_range.is_some());
    }

    #[test]
    fn group_leaves_get_distinct_source_byte_ranges() {
        let s = scope("demo", &[]);
        let got = bindings_from_use(&parse("use foo::{Bar, Baz};"), &s, Path::new(FAKE_FILE));
        assert_eq!(got.len(), 2);
        let by_name: std::collections::HashMap<_, _> = got
            .iter()
            .map(|b| (b.local_name.as_str(), b.source.as_ref().unwrap()))
            .collect();
        let bar = by_name["Bar"].byte_range.as_ref().unwrap();
        let baz = by_name["Baz"].byte_range.as_ref().unwrap();
        assert_ne!(bar, baz, "each leaf must carry its own byte range");
    }

    #[test]
    fn rename_anchors_at_canonical_ident_not_local_alias() {
        let s = scope("demo", &[]);
        let got = bindings_from_use(&parse("use foo::Bar as Quux;"), &s, Path::new(FAKE_FILE));
        assert_eq!(got.len(), 1);
        let span = got[0].source.as_ref().unwrap();
        // The canonical-side `Bar` ident should be the anchor. Its byte
        // range starts before the ` as Quux` suffix in the input source,
        // so the start offset must fall inside the `foo::Bar` portion.
        let br = span.byte_range.as_ref().unwrap();
        let source = "use foo::Bar as Quux;";
        let bar_start = source.find("Bar").unwrap() as u32;
        let bar_end = bar_start + "Bar".len() as u32;
        assert_eq!(br.start, bar_start);
        assert_eq!(br.end, bar_end);
    }

    // --- glob_targets_from_use ---

    #[test]
    fn glob_targets_extracts_simple_prefix() {
        let s = scope("demo", &[]);
        let targets = glob_targets_from_use(&parse("use foo::bar::*;"), &s);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].display(), "foo::bar");
    }

    #[test]
    fn glob_targets_emit_nothing_for_non_glob() {
        let s = scope("demo", &[]);
        let targets = glob_targets_from_use(&parse("use foo::Bar;"), &s);
        assert!(targets.is_empty());
    }

    #[test]
    fn glob_targets_emit_for_each_glob_in_group() {
        let s = scope("demo", &[]);
        let targets = glob_targets_from_use(&parse("use foo::{a::*, b::*};"), &s);
        let mut displays: Vec<_> = targets.iter().map(|p| p.display()).collect();
        displays.sort();
        assert_eq!(displays, vec!["foo::a", "foo::b"]);
    }

    #[test]
    fn glob_targets_apply_crate_prefix() {
        let s = scope("demo", &["sub"]);
        let targets = glob_targets_from_use(&parse("use crate::inner::*;"), &s);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].display(), "demo::inner");
    }
}
