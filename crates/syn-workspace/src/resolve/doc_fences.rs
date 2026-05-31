//! Doc-test code-fence scanning for the dependency lint.
//!
//! A dependency referenced only inside a doc-test example — a ```` ```rust ````
//! code fence in a `///` / `//!` doc comment — is still genuinely used: the
//! doc-test won't compile without it. But it appears nowhere in regular code, so
//! `unused-deps` would otherwise flag it as unused (the anyhow `futures` corpus
//! false positive).
//!
//! This module extracts the *crate-name* references from such fences. The result
//! feeds **only** the dependency lint (via [`crate::Workspace::doctest_dep_refs`]);
//! it is deliberately kept out of the occurrence / reference graph that
//! `unused-pub`, `architecture`, and the SCIP projection consume. Doc-test code
//! is a separate compilation unit, so its references are dependency-usage
//! evidence only — not references *from this crate's code*.
//!
//! Precision rules:
//! - Only fences rustdoc would compile as Rust are scanned: an empty info-string
//!   or rust markers (`no_run` / `should_panic` / `edition2021` / …). `text`,
//!   `ignore`, `compile_fail`, and other-language fences are skipped — their
//!   contents are non-Rust, unreliable, or intentionally invalid.
//! - Rustdoc hidden lines (`# `-prefixed) are real Rust and are scanned.
//! - Only `::`-qualified path *heads* (`dep` in `dep::Thing` or `dep::{A, B}`)
//!   are collected; bare single idents (locals, prelude names) are ignored. The
//!   heads need no resolution — they're already underscore-form crate names, and
//!   any that aren't a declared dependency are harmlessly ignored by the lint.
//!
//! Known limitation: block doc comments (`/** … */`) are not scanned.

use std::collections::HashSet;
use std::str::FromStr;

/// Crate-name references found in rust-compiling code fences within `source`'s
/// line doc comments (`///`, `//!`). See the module docs for scope and rationale.
pub(crate) fn doc_fence_crate_refs(source: &str) -> HashSet<String> {
    let mut refs = HashSet::new();
    let mut fence: Option<Fence> = None;
    let mut body = String::new();

    for line in source.lines() {
        let Some(doc) = doc_comment_body(line) else {
            // A non-doc line breaks the doc-comment block; a fence can't span
            // the gap, so close it conservatively.
            if let Some(f) = fence.take() {
                if f.scan {
                    scan_body(&body, &mut refs);
                }
                body.clear();
            }
            continue;
        };
        let trimmed = doc.trim_start();
        match (fence.as_ref(), fence_marker(trimmed)) {
            (None, Some((ch, len, info))) => {
                // Opening fence.
                fence = Some(Fence {
                    ch,
                    len,
                    scan: is_rust_fence(info),
                });
                body.clear();
            }
            (Some(f), Some((ch, len, info)))
                if ch == f.ch && len >= f.len && info.trim().is_empty() =>
            {
                // Closing fence (same marker, at least as long, no info string).
                let scan = f.scan;
                fence = None;
                if scan {
                    scan_body(&body, &mut refs);
                }
                body.clear();
            }
            (Some(f), _) => {
                // A line inside an open fence (incl. a fence-looking line that
                // isn't a valid close): body content.
                if f.scan {
                    push_body_line(&mut body, doc);
                }
            }
            (None, None) => {} // prose outside any fence — never scanned.
        }
    }
    // Unterminated fence at EOF — scan what we have.
    if let Some(f) = fence
        && f.scan
    {
        scan_body(&body, &mut refs);
    }
    refs
}

struct Fence {
    /// Fence marker character (`` ` `` or `~`).
    ch: char,
    /// Number of marker chars in the opening fence (close must be ≥ this).
    len: usize,
    /// Whether this fence's contents are scanned (a rust-compiling doc-test).
    scan: bool,
}

/// The body of a line doc comment (`///` or `//!`), with one optional leading
/// space stripped, or `None` if `line` is not a line doc comment. `////…` is a
/// regular comment, not a doc comment.
fn doc_comment_body(line: &str) -> Option<&str> {
    let t = line.trim_start();
    if let Some(rest) = t.strip_prefix("///") {
        if rest.starts_with('/') {
            return None; // `////…` is a normal comment.
        }
        return Some(rest.strip_prefix(' ').unwrap_or(rest));
    }
    if let Some(rest) = t.strip_prefix("//!") {
        return Some(rest.strip_prefix(' ').unwrap_or(rest));
    }
    None
}

/// If `trimmed` opens or closes a code fence, return `(marker_char, marker_len,
/// info_string)`. A fence is ≥3 backticks or tildes.
fn fence_marker(trimmed: &str) -> Option<(char, usize, &str)> {
    for ch in ['`', '~'] {
        let len = trimmed.chars().take_while(|&c| c == ch).count();
        if len >= 3 {
            // `ch` is single-byte ASCII, so the byte offset equals `len`.
            return Some((ch, len, &trimmed[len..]));
        }
    }
    None
}

/// Whether a fence with this info-string is Rust that rustdoc compiles. Empty →
/// yes; otherwise every comma/space-separated token must be a recognized rust
/// marker. `ignore` / `compile_fail` are excluded on purpose: rustdoc treats
/// them as Rust but does not run / require them to compile, so their references
/// are unreliable.
fn is_rust_fence(info: &str) -> bool {
    let mut tokens = info
        .split([',', ' ', '\t'])
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .peekable();
    if tokens.peek().is_none() {
        return true; // bare ``` → a Rust doc-test.
    }
    tokens.all(is_rust_marker)
}

fn is_rust_marker(tok: &str) -> bool {
    matches!(
        tok,
        "rust"
            | "should_panic"
            | "no_run"
            | "test_harness"
            | "allow_fail"
            | "edition2015"
            | "edition2018"
            | "edition2021"
            | "edition2024"
    )
}

/// Append a fence body line, stripping any rustdoc hidden-line marker (`# ` or a
/// bare `#`). Hidden lines are real Rust; their content is kept.
fn push_body_line(body: &mut String, doc: &str) {
    let rest = doc.trim_start();
    let code = if let Some(after) = rest.strip_prefix("# ") {
        after
    } else if rest == "#" {
        ""
    } else {
        doc
    };
    body.push_str(code);
    body.push('\n');
}

/// Tokenize a fence body and collect its `::`-qualified path heads.
fn scan_body(body: &str, refs: &mut HashSet<String>) {
    let Ok(stream) = proc_macro2::TokenStream::from_str(body) else {
        return; // not lexable as Rust tokens (e.g. an unbalanced snippet) — skip.
    };
    collect_path_heads(stream, refs);
}

/// Insert the leading segment of every `::`-qualified path (`dep` in
/// `dep::Thing`, `dep::{A, B}`, `::dep::Thing`). An ident is a head iff it is
/// followed by `::` and is not itself an interior segment (`prev::ident`).
fn collect_path_heads(stream: proc_macro2::TokenStream, refs: &mut HashSet<String>) {
    use proc_macro2::TokenTree;
    let tokens: Vec<TokenTree> = stream.into_iter().collect();
    let is_colon =
        |t: Option<&TokenTree>| matches!(t, Some(TokenTree::Punct(p)) if p.as_char() == ':');
    for (i, tok) in tokens.iter().enumerate() {
        match tok {
            TokenTree::Ident(id) => {
                let followed = is_colon(tokens.get(i + 1)) && is_colon(tokens.get(i + 2));
                // Interior segment: `Ident :: thisIdent` (the `::` before it is
                // preceded by another ident). A leading `::dep` has no such
                // ident, so its head is still collected.
                let interior = i >= 3
                    && is_colon(tokens.get(i - 1))
                    && is_colon(tokens.get(i - 2))
                    && matches!(tokens.get(i - 3), Some(TokenTree::Ident(_)));
                if followed && !interior {
                    let name = id.to_string();
                    if !matches!(name.as_str(), "crate" | "self" | "super" | "Self") {
                        refs.insert(name);
                    }
                }
            }
            TokenTree::Group(g) => collect_path_heads(g.stream(), refs),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::doc_fence_crate_refs;

    fn refs(src: &str) -> Vec<String> {
        let mut v: Vec<String> = doc_fence_crate_refs(src).into_iter().collect();
        v.sort();
        v
    }

    #[test]
    fn scans_use_in_a_bare_rust_fence() {
        // Mirrors the anyhow `futures` case: a dep referenced only in a `///`
        // example with a `futures::stream::{…}` import.
        let src = "\
/// Example:
///
/// ```
/// use futures::stream::{Stream, StreamExt};
/// fn demo<S: Stream>(_: S) {}
/// ```
pub fn f() {}
";
        assert_eq!(refs(src), vec!["futures".to_string()]);
    }

    #[test]
    fn catches_the_group_import_form() {
        // `use dep::{A, B}` — `dep` precedes a group, so a multi-segment-run
        // scan would miss it; the path-head rule catches it.
        let src = "/// ```\n/// use serde::{Serialize, Deserialize};\n/// ```\n";
        assert_eq!(refs(src), vec!["serde".to_string()]);
    }

    #[test]
    fn scans_hidden_lines() {
        // Rustdoc hidden lines (`# `) are real Rust and are scanned.
        let src = "/// ```\n/// # use tokio::runtime::Runtime;\n/// let _ = ();\n/// ```\n";
        assert_eq!(refs(src), vec!["tokio".to_string()]);
    }

    #[test]
    fn skips_non_rust_and_uncompiled_fences() {
        for info in ["text", "ignore", "compile_fail", "bash", "json"] {
            let src = format!("/// ```{info}\n/// use notacrate::Thing;\n/// ```\n");
            assert!(
                doc_fence_crate_refs(&src).is_empty(),
                "fence `{info}` should not be scanned"
            );
        }
    }

    #[test]
    fn ignores_prose_outside_fences() {
        // A `::`-qualified path in prose (not in a fence) is never scanned.
        let src = "/// See [`foo::bar`] and the module `baz::qux` for details.\npub fn f() {}\n";
        assert!(doc_fence_crate_refs(src).is_empty());
    }

    #[test]
    fn scans_inner_doc_and_rust_marker_fence() {
        // `//!` inner docs are scanned too; explicit `rust,no_run` marker fence.
        let src = "//! ```rust,no_run\n//! use rayon::prelude::*;\n//! ```\n";
        assert_eq!(refs(src), vec!["rayon".to_string()]);
    }

    #[test]
    fn skips_path_keywords_and_interior_segments() {
        // `crate::`/`self::` heads are dropped; only the leading segment of an
        // external path is kept (not interior `stream`).
        let src = "/// ```\n/// use crate::helpers::go;\n/// use futures::stream::iter;\n/// ```\n";
        assert_eq!(refs(src), vec!["futures".to_string()]);
    }
}
