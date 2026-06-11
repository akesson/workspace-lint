//! SCIP symbol normalization — project a `rust-analyzer scip` symbol onto the
//! canonical `Vec<String>` segment form `syn-workspace` uses, so the two can be
//! compared. Ported from `tools/oracle-bless` (`parse_symbol`); kept in lockstep
//! with `DESIGN-ir-pipeline.md` §8.
//!
//! Two normalizations matter here (the same the oracle harness applies):
//!
//! - **Package name** → cargo's hyphenated name becomes the code form
//!   (`md-5` → `md_5`), matching `ResolvedPath`'s leading crate segment.
//! - **Inherent / trait methods**, which rust-analyzer encodes with a synthetic
//!   `impl` descriptor before the `Self` type (`…/impl#[Type]method().`), get the
//!   `impl` marker dropped so the segments read `[…, Type, method]` — i.e.
//!   `impl#[T]m` → `T::m`. That makes a method reference prefix-matchable to the
//!   type it belongs to (a `Thing::new()` call counts as a use of `Thing`).
//!
//! The duplication with `tools/oracle-bless::parse_symbol` is intentional and
//! small; the bless tool lives in a detached workspace so the published library
//! can't depend on it. If you touch the descriptor handling, update both.

/// A SCIP symbol normalized to `syn-workspace`'s canonical segment form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NormalizedSymbol {
    /// Code-form package name (`-` → `_`); also `segments[0]`.
    pub package: String,
    /// `[package, ..descriptors]` with the `impl` desugaring marker removed.
    pub segments: Vec<String>,
}

/// Parse and normalize a SCIP symbol. Returns `None` for a local symbol (no
/// package) or an unparsable string — both are simply not reference evidence.
pub(crate) fn normalize_symbol(symbol: &str) -> Option<NormalizedSymbol> {
    let sym = scip::symbol::parse_symbol(symbol).ok()?;
    let package = sym.package.as_ref()?.name.replace('-', "_");
    if package.is_empty() {
        return None;
    }
    let mut segments = vec![package.clone()];
    for d in &sym.descriptors {
        // Drop the `impl` desugaring marker (`…/impl#[Type]method().`): the next
        // descriptor is the `Self` type, which stands in its place, collapsing
        // `impl#[T]method` to `T::method`.
        if d.name == "impl" {
            continue;
        }
        if !d.name.is_empty() {
            segments.push(d.name.clone());
        }
    }
    Some(NormalizedSymbol { package, segments })
}

/// `true` if `canonical` is a prefix of `symbol_segments` (equal counts as a
/// prefix). An exact match is a direct reference to the item; a proper prefix is
/// a reference to one of its members/methods (`Type::method` ⇒ `Type` is used).
/// This is the unused-pub disproof test once the symbol is normalized.
pub(crate) fn is_prefix(canonical: &[String], symbol_segments: &[String]) -> bool {
    canonical.len() <= symbol_segments.len()
        && canonical.iter().zip(symbol_segments).all(|(a, b)| a == b)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a SCIP symbol string. `descriptors` is the descriptor suffix-coded
    /// tail (e.g. `"mymod/Thing#"`). Scheme/manager/version are the rust-analyzer
    /// shape; `parse_symbol` only cares that the 5 space-delimited header fields
    /// are present.
    fn sym(pkg: &str, version: &str, descriptors: &str) -> String {
        format!("rust-analyzer cargo {pkg} {version} {descriptors}")
    }

    #[test]
    fn normalizes_type_symbol() {
        let n = normalize_symbol(&sym("md-5", "0.10.0", "Md5#")).unwrap();
        assert_eq!(n.package, "md_5", "package hyphen normalized");
        assert_eq!(n.segments, vec!["md_5", "Md5"]);
    }

    #[test]
    fn normalizes_module_path_to_a_type() {
        let n = normalize_symbol(&sym("my_crate", "0.1.0", "mymod/Thing#")).unwrap();
        assert_eq!(n.segments, vec!["my_crate", "mymod", "Thing"]);
    }

    #[test]
    fn collapses_impl_method_to_type_method() {
        // rust-analyzer encodes an inherent method as `mymod/impl#[Thing]new().`
        // — the `impl` marker is dropped so `Thing` precedes `new`.
        let n = normalize_symbol(&sym("my_crate", "0.1.0", "mymod/impl#[Thing]new().")).unwrap();
        assert_eq!(n.segments, vec!["my_crate", "mymod", "Thing", "new"]);
    }

    #[test]
    fn local_symbol_without_package_is_none() {
        // A local symbol starts with `local ` and carries no package.
        assert!(normalize_symbol("local 1").is_none());
    }

    #[test]
    fn unparsable_symbol_is_none() {
        assert!(normalize_symbol("").is_none());
        assert!(normalize_symbol("garbage").is_none());
    }

    #[test]
    fn non_ascii_ident_is_unescaped() {
        // SCIP backtick-wraps non-ASCII idents; `parse_symbol` un-escapes them.
        // Guards the café UTF-8 case (DESIGN §8 regression guard).
        let n = normalize_symbol(&sym("my_crate", "0.1.0", "`café`#")).unwrap();
        assert_eq!(n.segments, vec!["my_crate", "café"]);
    }

    #[test]
    fn prefix_matching() {
        let canonical = vec!["c".to_string(), "m".to_string(), "Thing".to_string()];
        // Exact match (direct reference).
        assert!(is_prefix(&canonical, &canonical));
        // Proper prefix (method reference).
        let method = vec![
            "c".to_string(),
            "m".to_string(),
            "Thing".to_string(),
            "new".to_string(),
        ];
        assert!(is_prefix(&canonical, &method));
        // Different item — not a prefix.
        let other = vec!["c".to_string(), "m".to_string(), "Other".to_string()];
        assert!(!is_prefix(&canonical, &other));
        // Symbol shorter than canonical — not a prefix.
        assert!(!is_prefix(&canonical, &["c".to_string()]));
    }
}
