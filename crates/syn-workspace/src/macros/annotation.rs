//! Layer 2: explicit annotations on macro definitions.
//!
//! This layer recognises two equivalent forms that declare the paths a macro's
//! expansion will reference, placed immediately before the `macro_rules!` body:
//!
//! 1. The **`expansion_uses!(...)` marker macro** (needs the zero-dep
//!    `syn-workspace-marker` crate):
//!
//!    ```ignore
//!    my_marker::expansion_uses!(serde::Serialize, chrono::DateTime);
//!    macro_rules! my_macro { /* ... */ }
//!    ```
//!
//! 2. The dependency-free **`// workspace-syn: expansion-uses(...)` comment
//!    directive** (mirrors the `# workspace-lint: allow(...)` style):
//!
//!    ```ignore
//!    // workspace-syn: expansion-uses(serde::Serialize, chrono::DateTime)
//!    macro_rules! my_macro { /* ... */ }
//!    ```
//!
//! Both feed the same path list through the Layer-1 `extract_macro_paths`
//! token scan, so they produce identical `Origin::Macro` occurrences. The macro
//! form is matched on the `syn` AST during the walk (`is_expansion_uses`); the
//! comment form is recovered from raw file text
//! (`comment_expansion_uses_occurrences`) because line comments aren't in the
//! AST.
//!
//! The marker-crate names recognized as the leading segment of an
//! `expansion_uses!` invocation are configurable via
//! [`crate::Workspace::load_with_options`]; by default the names
//! `workspace_syn` and `syn_workspace_marker` are accepted (these match
//! the historical defaults). Restricting the prefix avoids treating a
//! third-party `foo::expansion_uses!` as a Layer 2 annotation, which
//! would silently feed its body into the implicit-refs set.

use std::path::Path;

use crate::macros::autodetect::extract_macro_paths;
use crate::resolve::Occurrence;

/// Leading marker of the comment-directive form. Hyphenated (`workspace-syn`,
/// not the `workspace_syn` crate name) to match the published documentation and
/// the sibling `workspace-lint:` directive style.
const COMMENT_DIRECTIVE_MARKER: &str = "workspace-syn:";

/// Match `expansion_uses!` (unqualified) or `<crate>::expansion_uses!`
/// where the leading segment matches one of `marker_crates`. The
/// unqualified form always matches (it can't be confused with a
/// third-party macro because there's no prefix to attribute it to).
pub(crate) fn is_expansion_uses(path: &syn::Path, marker_crates: &[String]) -> bool {
    let segs: Vec<&syn::Ident> = path.segments.iter().map(|s| &s.ident).collect();
    match segs.as_slice() {
        [single] => *single == "expansion_uses",
        [krate, name] => {
            *name == "expansion_uses" && marker_crates.iter().any(|m| *krate == m.as_str())
        }
        _ => false,
    }
}

/// Scan raw source text for the comment-directive form
/// (`// workspace-syn: expansion-uses(path, ...)`) and lower each captured path
/// list to `Origin::Macro` occurrences via the same [`extract_macro_paths`]
/// scan the `expansion_uses!` macro arguments use — so the two forms are
/// behaviourally identical. The caller seeds the result into the file's
/// top-level module, where it is resolved by the central Phase-B pass like any
/// other occurrence.
///
/// Scoping note: every directive in a file attaches to the file's top-level
/// module. The realistic annotation is a fully-qualified path (`serde::Serialize`,
/// `crate::Foo`), where scope is irrelevant. A directive inside an inline
/// `mod { … }` that names a path relative to *that* module would resolve against
/// the file top instead — a rare case, and additive-only (a missed suppression,
/// never a false positive, since occurrences only *add* references).
pub(crate) fn comment_expansion_uses_occurrences(source: &str, file: &Path) -> Vec<Occurrence> {
    let mut out = Vec::new();
    for line in source.lines() {
        let Some(path_list) = directive_path_list(line) else {
            continue;
        };
        // The captured list is identical token content to the macro form's
        // arguments — reuse the same scan. A list that doesn't tokenize is a
        // malformed directive and contributes nothing.
        if let Ok(tokens) = path_list.parse::<proc_macro2::TokenStream>() {
            extract_macro_paths(tokens, file, &mut out);
        }
    }
    out
}

/// Extract the `<inner>` of a `// workspace-syn: expansion-uses(<inner>)`
/// directive from one source line, or `None` if the line carries none.
///
/// The directive must be the content of a line comment (`//`, or `#` for
/// non-Rust callers) — exactly as `workspace-lint`'s own directive scanner
/// requires. Anchoring *after* the comment leader (rather than matching the
/// marker anywhere on the line) is what stops the resolver from treating a
/// mention of the directive inside doc-comment prose or a string literal —
/// including this crate's own docs and tests — as if it were live.
fn directive_path_list(line: &str) -> Option<&str> {
    let line = line.trim_start();
    let body = line
        .strip_prefix("//")
        .or_else(|| line.strip_prefix('#'))?
        .trim_start();
    let after_marker = body.strip_prefix(COMMENT_DIRECTIVE_MARKER)?.trim_start();
    let after_kind = after_marker.strip_prefix("expansion-uses")?.trim_start();
    let inner = after_kind.strip_prefix('(')?;
    let close = inner.find(')')?;
    Some(&inner[..close])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::Origin;

    fn paths(src: &str) -> Vec<String> {
        let mut out: Vec<String> = comment_expansion_uses_occurrences(src, Path::new("<test>"))
            .iter()
            .map(|o| o.segments.join("::"))
            .collect();
        out.sort();
        out
    }

    #[test]
    fn directive_path_list_extracts_inner() {
        assert_eq!(
            directive_path_list("// workspace-syn: expansion-uses(a::B, c::D)"),
            Some("a::B, c::D")
        );
    }

    #[test]
    fn no_directive_returns_none() {
        assert_eq!(directive_path_list("// just a comment"), None);
        assert_eq!(directive_path_list("let x = foo();"), None);
        assert_eq!(directive_path_list("// workspace-syn: allow(thing)"), None);
    }

    #[test]
    fn scans_multi_segment_paths_as_macro_origin() {
        let src = "// workspace-syn: expansion-uses(serde::Serialize, chrono::DateTime)\n\
                   macro_rules! m { () => {}; }\n";
        assert_eq!(paths(src), vec!["chrono::DateTime", "serde::Serialize"]);
        let occ = comment_expansion_uses_occurrences(src, Path::new("<test>"));
        assert!(occ.iter().all(|o| o.origin == Origin::Macro));
    }

    #[test]
    fn single_idents_are_dropped_like_the_macro_form() {
        // extract_macro_paths keeps only multi-segment runs, so a bare name —
        // which the macro form also drops — contributes nothing here too.
        assert!(paths("// workspace-syn: expansion-uses(Bare)").is_empty());
    }

    #[test]
    fn malformed_directive_is_ignored() {
        assert!(paths("// workspace-syn: expansion-uses(").is_empty());
        assert!(paths("// workspace-syn: something-else(a::B)").is_empty());
    }

    #[test]
    fn requires_a_comment_leader() {
        // The directive must be a line comment's content. A bare line that merely
        // mentions the marker — or a doc comment (`//!`/`///`) discussing it — is
        // not scanned, so the resolver never treats its own docs/tests as live.
        assert_eq!(
            paths("// workspace-syn: expansion-uses(a::B)"),
            vec!["a::B"]
        );
        assert!(paths("  workspace-syn: expansion-uses(a::B)").is_empty());
        assert!(paths("//! // workspace-syn: expansion-uses(a::B)").is_empty());
        assert!(paths("/// workspace-syn: expansion-uses(a::B)").is_empty());
    }
}
