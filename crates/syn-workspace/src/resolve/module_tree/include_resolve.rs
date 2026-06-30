//! Resolve the on-disk file an `include!(...)` invocation pulls in.
//!
//! `include!` is how Rust splices generated code into a crate
//! (`include!(concat!(env!("OUT_DIR"), "/gen.rs"))`, `include!("table.rs")`).
//! This module is its analogue of [`resolve_mod_file`](super::resolve_mod_file):
//! it const-folds the argument expression — string literals, `concat!`, `env!`
//! — against a provided environment map, then joins the result to the including
//! file's directory.
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
/// environment used to fold `env!(...)` (seeded with `CARGO_*` vars, optionally
/// overlaid with a crate's harvested build-script env), the current include
/// nesting depth, and the set of `include!`d files already on the current chain.
/// `Copy` so it threads through the walk's recursion without ceremony.
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
    /// `walk.rs` and call this.
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

#[cfg(test)]
impl IncludeCtx<'static> {
    /// A context with no environment, zero depth, and no ancestry — used by tests
    /// and the `#[cfg(test)]` `build_crate_tree` helper. Resolves only literal
    /// includes. Production loads use [`IncludeCtx::root`] with a real env.
    pub(crate) fn none() -> Self {
        static EMPTY_ENV: std::sync::LazyLock<HashMap<String, String>> =
            std::sync::LazyLock::new(HashMap::new);
        IncludeCtx::root(&EMPTY_ENV)
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
    fn nested_concat() {
        assert_eq!(
            fold(r#"concat!("a", concat!("b", "c"))"#, &env(&[])).as_deref(),
            Some("abc")
        );
    }

    #[test]
    fn env_present() {
        assert_eq!(
            fold(r#"env!("OUT_DIR")"#, &env(&[("OUT_DIR", "/out")])).as_deref(),
            Some("/out")
        );
    }

    #[test]
    fn env_absent_is_none() {
        assert_eq!(fold(r#"env!("MISSING")"#, &env(&[])), None);
    }

    #[test]
    fn env_two_arg_form_ignores_message() {
        assert_eq!(
            fold(r#"env!("X", "set X in build.rs")"#, &env(&[("X", "v")])).as_deref(),
            Some("v")
        );
    }

    #[test]
    fn qualified_core_concat() {
        assert_eq!(
            fold(r#"core::concat!("a", "b")"#, &env(&[])).as_deref(),
            Some("ab")
        );
    }

    #[test]
    fn qualified_std_env() {
        assert_eq!(
            fold(r#"std::env!("X")"#, &env(&[("X", "v")])).as_deref(),
            Some("v")
        );
    }

    #[test]
    fn out_dir_concat_pattern() {
        // The canonical prost/tonic shape.
        assert_eq!(
            fold(
                r#"concat!(env!("OUT_DIR"), "/proto.rs")"#,
                &env(&[("OUT_DIR", "/target/build/x/out")])
            )
            .as_deref(),
            Some("/target/build/x/out/proto.rs")
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
    fn empty_concat_is_empty_string() {
        assert_eq!(fold(r#"concat!()"#, &env(&[])).as_deref(), Some(""));
    }

    #[test]
    fn non_literal_arg_is_none() {
        assert_eq!(fold(r#"some_const"#, &env(&[])), None);
        assert_eq!(fold(r#"42"#, &env(&[])), None);
        // A `concat!` containing a non-string operand can't be folded for a path.
        assert_eq!(fold(r#"concat!("a", 1)"#, &env(&[])), None);
    }

    #[test]
    fn unrecognized_macro_is_none() {
        assert_eq!(fold(r#"format!("{}", "x")"#, &env(&[])), None);
    }

    #[test]
    fn macro_is_matches_bare_and_qualified() {
        let p: syn::Path = syn::parse_str("include").unwrap();
        assert!(macro_is(&p, "include"));
        let p: syn::Path = syn::parse_str("std::include").unwrap();
        assert!(macro_is(&p, "include"));
        let p: syn::Path = syn::parse_str("core::include").unwrap();
        assert!(macro_is(&p, "include"));
        let p: syn::Path = syn::parse_str("foo::include").unwrap();
        assert!(!macro_is(&p, "include"));
        let p: syn::Path = syn::parse_str("a::b::include").unwrap();
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
    fn resolve_absolute_passthrough() {
        let dir = tempfile::tempdir().unwrap();
        let abs = dir.path().join("abs.rs");
        std::fs::write(&abs, "pub fn g() {}").unwrap();
        // A different base_dir must be ignored for an absolute include path.
        let other = tempfile::tempdir().unwrap();
        // Build the macro from a `LitStr` *value*, not by formatting the path
        // into source text: a Windows absolute path (`C:\…`) embedded in a string
        // literal contains invalid Rust escapes and won't tokenize. Since
        // `resolve_include_path` only reads `mac.tokens` (parsed as a `syn::Expr`),
        // a macro whose tokens are a single `LitStr` is all this needs.
        let lit = syn::LitStr::new(abs.to_str().unwrap(), proc_macro2::Span::call_site());
        let mac: syn::Macro = syn::parse_quote!(include!(#lit));
        let got = resolve_include_path(&mac, other.path(), &env(&[]));
        assert_eq!(got.as_deref(), Some(abs.as_path()));
    }

    #[test]
    fn resolve_out_dir_pattern() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        std::fs::create_dir(&out).unwrap();
        std::fs::write(out.join("proto.rs"), "pub struct M;").unwrap();
        let mac = include_macro(r#"concat!(env!("OUT_DIR"), "/proto.rs")"#);
        let env = env(&[("OUT_DIR", out.to_str().unwrap())]);
        let got = resolve_include_path(&mac, dir.path(), &env);
        assert_eq!(got.as_deref(), Some(out.join("proto.rs").as_path()));
    }

    #[test]
    fn resolve_nonexistent_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let mac = include_macro(r#""does_not_exist.rs""#);
        assert_eq!(resolve_include_path(&mac, dir.path(), &env(&[])), None);
    }

    #[test]
    fn resolve_unresolvable_arg_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let mac = include_macro(r#"env!("UNSET_VAR")"#);
        assert_eq!(resolve_include_path(&mac, dir.path(), &env(&[])), None);
    }
}
