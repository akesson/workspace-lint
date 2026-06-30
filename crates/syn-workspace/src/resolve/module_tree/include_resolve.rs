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

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use syn::punctuated::Punctuated;

/// Hard cap on `include!` nesting depth. Real generated code nests one or two
/// levels at most; the cap only exists so a (compile-invalid) cyclic
/// `include!` can't drive the resolver into unbounded recursion.
pub(crate) const MAX_INCLUDE_DEPTH: usize = 64;

/// Context threaded through the module-tree walk for `include!` resolution: the
/// environment used to fold `env!(...)` (seeded with `CARGO_*` vars, optionally
/// overlaid with a crate's harvested build-script env) and the current include
/// nesting depth (the cycle backstop). `Copy` so it threads through the walk's
/// recursion without ceremony.
#[derive(Clone, Copy)]
pub(crate) struct IncludeCtx<'a> {
    pub env: &'a HashMap<String, String>,
    pub depth: usize,
}

#[cfg(test)]
impl IncludeCtx<'static> {
    /// A context with no environment and zero depth — used by tests and the
    /// `#[cfg(test)]` `build_crate_tree` helper. Resolves only literal includes.
    /// Production loads always build a real `CARGO_*`-seeded env in `walk.rs`.
    pub(crate) fn none() -> Self {
        static EMPTY: std::sync::LazyLock<HashMap<String, String>> =
            std::sync::LazyLock::new(HashMap::new);
        IncludeCtx {
            env: &EMPTY,
            depth: 0,
        }
    }
}

impl<'a> IncludeCtx<'a> {
    /// The context one `include!` level deeper — used when recursing into an
    /// included file. Returns `None` once [`MAX_INCLUDE_DEPTH`] is reached.
    pub(crate) fn deeper(self) -> Option<Self> {
        (self.depth < MAX_INCLUDE_DEPTH).then_some(IncludeCtx {
            env: self.env,
            depth: self.depth + 1,
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
        let mac = include_macro(&format!(r#""{}""#, abs.display()));
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
