//! Resolve the on-disk file an `include!(...)` invocation pulls in.
//!
//! `include!` is how Rust splices generated code into a crate
//! (`include!(concat!(env!("OUT_DIR"), "/gen.rs"))`, `include!("table.rs")`).
//! This module is its analogue of `resolve_mod_file` (in the sibling
//! `module_tree`): it const-folds the argument expression — string literals,
//! `concat!`, `env!` — against a provided environment map, then joins the
//! result to the including file's directory.
//!
//! Copied verbatim from syn-workspace's `resolve::module_tree::include_resolve`
//! (the duplication is deliberate and dies when syn-workspace retires). The
//! fast tier seeds the environment with `CARGO_*` vars only — no build-script
//! harvest — so `OUT_DIR`-based includes stay unresolved here by design.
//!
//! Deliberately best-effort: any shape it can't statically reduce (a non-literal
//! operand, a `concat!` of a non-string, an absent env var) yields `None`, never
//! a panic. The caller then simply leaves the `include!` site un-spliced, which
//! is the pre-existing behavior — so an unresolvable include can only ever fail
//! to *improve* on today, never regress it.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use syn::punctuated::Punctuated;

/// Hard cap on `include!` nesting depth. With the [`IncludeCtx::ancestry`] cycle
/// set this is only a secondary backstop; it still bounds pathologically deep
/// (but acyclic) generated include chains.
pub(crate) const MAX_INCLUDE_DEPTH: usize = 64;

/// Context threaded through the module-tree walk for `include!` resolution: the
/// environment used to fold `env!(...)` (seeded with `CARGO_*` vars), the
/// current include nesting depth, and the set of `include!`d files already on
/// the current chain. `Copy` so it threads through the walk's recursion without
/// ceremony.
#[derive(Clone, Copy)]
pub(crate) struct IncludeCtx<'a> {
    pub env: &'a HashMap<String, String>,
    pub depth: usize,
    /// Canonicalized paths of the `include!`d files on the current include chain
    /// (the ancestors of the file being walked). A path already in this set is
    /// skipped rather than re-spliced, which breaks cyclic and *fan-out* cyclic
    /// includes (the depth cap alone bounds depth but not the exponential
    /// re-splicing a branching cycle would otherwise cause). Chain-scoped, not
    /// global: two unrelated modules may each legitimately include the same
    /// generated file.
    pub ancestry: &'a HashSet<PathBuf>,
}

/// The empty `include!` ancestry, shared by every chain root (no ancestors yet).
static EMPTY_ANCESTRY: std::sync::LazyLock<HashSet<PathBuf>> =
    std::sync::LazyLock::new(HashSet::new);

impl<'a> IncludeCtx<'a> {
    /// A root context for a crate walk: the given `CARGO_*`-seeded env, zero
    /// depth, and an empty include ancestry. Production loads build the env in
    /// `module_tree::build_targets` and call this.
    pub(crate) fn root(env: &'a HashMap<String, String>) -> Self {
        IncludeCtx {
            env,
            depth: 0,
            ancestry: &EMPTY_ANCESTRY,
        }
    }

    /// The context for descending into an `include!`d file, with `child_ancestry`
    /// = this chain plus the included file's canonical path. Returns `None` at
    /// [`MAX_INCLUDE_DEPTH`]. The caller owns `child_ancestry` so the returned
    /// borrow outlives the recursive walk into the included file.
    pub(crate) fn descend<'b>(&self, child_ancestry: &'b HashSet<PathBuf>) -> Option<IncludeCtx<'b>>
    where
        'a: 'b,
    {
        (self.depth < MAX_INCLUDE_DEPTH).then_some(IncludeCtx {
            env: self.env,
            depth: self.depth + 1,
            ancestry: child_ancestry,
        })
    }
}

/// True iff `path` names the macro `name`, accepting the bare form as well as
/// the `core::name` / `std::name` qualified spellings.
pub(crate) fn macro_is(path: &syn::Path, name: &str) -> bool {
    let Some(last) = path.segments.last() else {
        return false;
    };
    if last.ident != name {
        return false;
    }
    match path.segments.len() {
        1 => true,
        2 => {
            let first = &path.segments[0].ident;
            first == "core" || first == "std"
        }
        _ => false,
    }
}

/// Resolve an `include!` invocation to a real, existing file path, or `None` if
/// its argument can't be const-folded to an existing path.
///
/// `base_dir` is the directory of the file that contains the `include!` token —
/// Rust resolves a relative include path relative to that file.
pub(crate) fn resolve_include_path(
    mac: &syn::Macro,
    base_dir: &Path,
    env: &HashMap<String, String>,
) -> Option<PathBuf> {
    let expr: syn::Expr = syn::parse2(mac.tokens.clone()).ok()?;
    let folded = fold_str_expr(&expr, env)?;
    let folded = Path::new(&folded);
    let candidate = if folded.is_absolute() {
        folded.to_path_buf()
    } else {
        base_dir.join(folded)
    };
    candidate.exists().then_some(candidate)
}

/// Const-fold a string-valued expression as used in an `include!` argument:
/// string literals, `concat!(...)`, `env!("VAR")`, and parenthesized/grouped
/// forms. Returns `None` for anything that can't be statically reduced to a
/// string (a non-string operand, an absent env var, an unrecognized macro).
pub(crate) fn fold_str_expr(expr: &syn::Expr, env: &HashMap<String, String>) -> Option<String> {
    match expr {
        syn::Expr::Lit(lit) => match &lit.lit {
            syn::Lit::Str(s) => Some(s.value()),
            _ => None,
        },
        syn::Expr::Group(g) => fold_str_expr(&g.expr, env),
        syn::Expr::Paren(p) => fold_str_expr(&p.expr, env),
        syn::Expr::Macro(m) => fold_macro(&m.mac, env),
        _ => None,
    }
}

fn fold_macro(mac: &syn::Macro, env: &HashMap<String, String>) -> Option<String> {
    if macro_is(&mac.path, "concat") {
        let mut out = String::new();
        for arg in parse_args(mac)? {
            out.push_str(&fold_str_expr(&arg, env)?);
        }
        Some(out)
    } else if macro_is(&mac.path, "env") {
        // `env!("VAR")` and the two-arg `env!("VAR", "error message")` — the
        // error message (if any) is irrelevant to path resolution.
        let args = parse_args(mac)?;
        let name = fold_str_expr(args.first()?, env)?;
        env.get(&name).cloned()
    } else {
        None
    }
}

/// Parse a macro body as a comma-separated list of expressions (the shape of
/// both `concat!` and `env!` arguments). Trailing commas are accepted.
fn parse_args(mac: &syn::Macro) -> Option<Vec<syn::Expr>> {
    mac.parse_body_with(Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated)
        .ok()
        .map(|p| p.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn fold(src: &str, env: &HashMap<String, String>) -> Option<String> {
        let expr: syn::Expr = syn::parse_str(src).expect("parse expr");
        fold_str_expr(&expr, env)
    }

    #[test]
    fn bare_string_literal() {
        assert_eq!(
            fold(r#""generated.rs""#, &env(&[])).as_deref(),
            Some("generated.rs")
        );
    }

    #[test]
    fn concat_of_literals() {
        assert_eq!(
            fold(r#"concat!("a", "b")"#, &env(&[])).as_deref(),
            Some("ab")
        );
    }

    #[test]
    fn env_present_and_absent() {
        assert_eq!(
            fold(r#"env!("OUT_DIR")"#, &env(&[("OUT_DIR", "/out")])).as_deref(),
            Some("/out")
        );
        assert_eq!(fold(r#"env!("MISSING")"#, &env(&[])), None);
    }

    #[test]
    fn qualified_spellings_fold() {
        assert_eq!(
            fold(r#"core::concat!("a", "b")"#, &env(&[])).as_deref(),
            Some("ab")
        );
        assert_eq!(
            fold(r#"std::env!("X")"#, &env(&[("X", "v")])).as_deref(),
            Some("v")
        );
    }

    #[test]
    fn manifest_dir_concat_pattern() {
        assert_eq!(
            fold(
                r#"concat!(env!("CARGO_MANIFEST_DIR"), "/src/generated.rs")"#,
                &env(&[("CARGO_MANIFEST_DIR", "/m")])
            )
            .as_deref(),
            Some("/m/src/generated.rs")
        );
    }

    #[test]
    fn non_literal_or_unrecognized_is_none() {
        assert_eq!(fold(r#"some_const"#, &env(&[])), None);
        assert_eq!(fold(r#"42"#, &env(&[])), None);
        // A `concat!` containing a non-string operand can't be folded for a path.
        assert_eq!(fold(r#"concat!("a", 1)"#, &env(&[])), None);
        assert_eq!(fold(r#"format!("{}", "x")"#, &env(&[])), None);
    }

    #[test]
    fn macro_is_matches_bare_and_qualified() {
        let p: syn::Path = syn::parse_str("include").unwrap();
        assert!(macro_is(&p, "include"));
        let p: syn::Path = syn::parse_str("std::include").unwrap();
        assert!(macro_is(&p, "include"));
        let p: syn::Path = syn::parse_str("foo::include").unwrap();
        assert!(!macro_is(&p, "include"));
    }

    fn include_macro(arg_src: &str) -> syn::Macro {
        syn::parse_str(&format!("include!({arg_src})")).expect("parse include! macro")
    }

    #[test]
    fn resolve_relative_join() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("gen.rs"), "pub fn g() {}").unwrap();
        let mac = include_macro(r#""gen.rs""#);
        let got = resolve_include_path(&mac, dir.path(), &env(&[]));
        assert_eq!(got.as_deref(), Some(dir.path().join("gen.rs").as_path()));
    }

    #[test]
    fn resolve_nonexistent_or_unresolvable_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let mac = include_macro(r#""does_not_exist.rs""#);
        assert_eq!(resolve_include_path(&mac, dir.path(), &env(&[])), None);
        let mac = include_macro(r#"env!("UNSET_VAR")"#);
        assert_eq!(resolve_include_path(&mac, dir.path(), &env(&[])), None);
    }
}
