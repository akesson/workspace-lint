//! The `--fix` deletion suggestion for a stale `expect` directive.
//!
//! A stale directive is resolved by removing it — the mechanical inverse of
//! writing a silence directive (which `--fix` never does on the author's
//! behalf). We build a `MachineApplicable`, empty-replacement [`Suggestion`]
//! whose byte span covers the whole directive line(s), leading indent through
//! the trailing `\n` / `\r\n`, so the line disappears with no residue. The
//! shape mirrors `unused_deps::ir::build_delete_suggestion`.
//!
//! A guard (`snippet_is_directive_only`) re-checks that the bytes we're about
//! to remove are *exactly* a directive and nothing else, so a malformed origin
//! (a macro sharing its line with code, an unterminated `<!-- … -->` opener)
//! degrades to a help-only diagnostic instead of clobbering adjacent source.

use std::path::Path;

use fs_err as fs;

use super::{DirectiveOrigin, directive_regex, parse_workspace_lint_directive};
use wl_diagnostic::{Applicability, Span, Suggestion};

/// Build the whole-line deletion suggestion for the stale `expect` directive at
/// `origin`, or `None` (leaving the diagnostic help-only) when the file can't
/// be read, the recorded line range is out of bounds, or the extracted snippet
/// isn't exactly a directive. Callers only ask when *every* lint the directive
/// names is stale, so a successful deletion never removes a live silence.
pub(crate) fn deletion_suggestion(root: &Path, origin: &DirectiveOrigin) -> Option<Suggestion> {
    let abs = root.join(&origin.file);
    let content = fs::read_to_string(&abs).ok()?;
    let (byte_start, byte_end) = line_span(&content, origin.line, origin.line_end)?;
    // Trailing EOL kept in the span (so the line vanishes) but stripped from
    // `original`, so the human renderer prints a clean single-line `-` diff.
    let deleted = &content[byte_start..byte_end];
    let snippet = deleted.trim_end_matches(['\r', '\n']);
    let ext = origin.file.extension().and_then(|e| e.to_str());
    if !snippet_is_directive_only(snippet, ext) {
        return None;
    }
    Some(Suggestion {
        span: Span {
            file: abs,
            line_start: origin.line,
            line_end: origin.line_end,
            col_start: 1,
            col_end: 1,
            byte_start: byte_start as u32,
            byte_end: byte_end as u32,
        },
        message: "remove this stale expect directive".to_string(),
        replacement: String::new(),
        applicability: Applicability::MachineApplicable,
        original: Some(snippet.to_string()),
    })
}

/// Byte range `[start, end)` covering source lines `start_line..=end_line`
/// (1-based) *including* the trailing line terminator (`\n` or `\r\n`), or
/// through EOF for an unterminated final line. `None` if the range is degenerate
/// or runs past the end of the file. CRLF is handled for free: the `\r` lives
/// inside the line, so the range swallows the whole `\r\n`.
fn line_span(content: &str, start_line: u32, end_line: u32) -> Option<(usize, usize)> {
    if start_line == 0 || end_line < start_line {
        return None;
    }
    let bytes = content.as_bytes();
    // Byte offset where each line begins; `line_starts[n]` starts line `n + 1`.
    let mut line_starts = vec![0usize];
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\n' {
            line_starts.push(i + 1);
        }
    }
    let s = start_line as usize - 1;
    let e = end_line as usize - 1;
    if s >= line_starts.len() || e >= line_starts.len() {
        return None;
    }
    let byte_start = line_starts[s];
    // End of `end_line` = start of the next line, or EOF for the last line.
    let byte_end = line_starts.get(e + 1).copied().unwrap_or(bytes.len());
    if byte_end <= byte_start {
        return None;
    }
    Some((byte_start, byte_end))
}

/// Guard: `snippet` (the bytes about to be deleted, trailing EOL already
/// stripped) must be *exactly* a suppression directive so whole-line deletion
/// can't clobber adjacent code. Dispatches on the origin file's extension —
/// directive-syntax knowledge stays in the `directives` module.
fn snippet_is_directive_only(snippet: &str, ext: Option<&str>) -> bool {
    match ext {
        Some("rs") => {
            if snippet.trim_start().starts_with("//") {
                // Line-comment form: a lone directive comment.
                is_comment_directive_line(snippet)
            } else {
                // Macro form: exactly one item, and it's a directive macro.
                // Code sharing the line parses as ≥2 items (or fails), → reject.
                let Ok(file) = syn::parse_str::<syn::File>(snippet) else {
                    return false;
                };
                let [syn::Item::Macro(m)] = file.items.as_slice() else {
                    return false;
                };
                parse_workspace_lint_directive(&m.mac).is_some()
            }
        }
        Some("toml") | Some("md") => {
            // A single comment line. Reject an unterminated HTML-comment opener:
            // deleting only its first line would leave a dangling `-->`.
            if snippet.trim_start().starts_with("<!--") && !snippet.contains("-->") {
                return false;
            }
            is_comment_directive_line(snippet)
        }
        _ => false,
    }
}

/// True when `line` is a lone comment (`#`, `//`, `<!--`, or bare) whose body
/// is a `workspace-lint: allow|expect(...)` directive — the shape the text
/// scanners ([`super::scan_text`] / [`super::scan_rust_comments`]) accept.
fn is_comment_directive_line(line: &str) -> bool {
    let body = line
        .trim_start()
        .trim_start_matches('#')
        .trim_start_matches("//")
        .trim_start_matches("<!--")
        .trim();
    directive_regex().is_match(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn origin(file: &str, line: u32, line_end: u32) -> DirectiveOrigin {
        DirectiveOrigin {
            file: PathBuf::from(file),
            line,
            line_end,
        }
    }

    fn write(dir: &Path, name: &str, content: &str) {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    // --- line_span ---

    #[test]
    fn line_span_single_line_lf() {
        // Line 2 of three, LF-terminated.
        let (s, e) = line_span("aaa\nbbb\nccc\n", 2, 2).unwrap();
        assert_eq!(&"aaa\nbbb\nccc\n"[s..e], "bbb\n");
    }

    #[test]
    fn line_span_swallows_crlf() {
        let content = "aaa\r\nbbb\r\nccc\r\n";
        let (s, e) = line_span(content, 1, 1).unwrap();
        assert_eq!(&content[s..e], "aaa\r\n");
    }

    #[test]
    fn line_span_last_line_without_trailing_newline() {
        let content = "aaa\nbbb";
        let (s, e) = line_span(content, 2, 2).unwrap();
        assert_eq!(&content[s..e], "bbb");
    }

    #[test]
    fn line_span_multi_line() {
        let content = "aaa\nbbb\nccc\nddd\n";
        let (s, e) = line_span(content, 2, 3).unwrap();
        assert_eq!(&content[s..e], "bbb\nccc\n");
    }

    #[test]
    fn line_span_out_of_range_is_none() {
        assert_eq!(line_span("aaa\n", 5, 5), None);
        assert_eq!(line_span("aaa\n", 0, 0), None);
        assert_eq!(line_span("aaa\n", 3, 2), None);
    }

    // --- deletion_suggestion: happy paths ---

    #[test]
    fn deletes_toml_comment_line_including_indent_and_eol() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "crates/demo/Cargo.toml",
            "[dependencies]\n    # workspace-lint: expect(centralized-deps)\nserde = \"1\"\n",
        );
        let sug = deletion_suggestion(tmp.path(), &origin("crates/demo/Cargo.toml", 2, 2)).unwrap();
        assert_eq!(sug.replacement, "");
        assert_eq!(sug.applicability, Applicability::MachineApplicable);
        assert_eq!(
            sug.original.as_deref(),
            Some("    # workspace-lint: expect(centralized-deps)")
        );
        // Applying the deletion removes the whole line, indent and newline.
        let content = std::fs::read_to_string(tmp.path().join("crates/demo/Cargo.toml")).unwrap();
        let mut fixed = content.clone();
        fixed.replace_range(sug.span.byte_start as usize..sug.span.byte_end as usize, "");
        assert_eq!(fixed, "[dependencies]\nserde = \"1\"\n");
    }

    #[test]
    fn deletes_rust_macro_directive_line() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "src/lib.rs",
            "workspace_lint::expect!(unused_pub);\npub fn f() {}\n",
        );
        let sug = deletion_suggestion(tmp.path(), &origin("src/lib.rs", 1, 1)).unwrap();
        assert_eq!(
            sug.original.as_deref(),
            Some("workspace_lint::expect!(unused_pub);")
        );
        let content = std::fs::read_to_string(tmp.path().join("src/lib.rs")).unwrap();
        let mut fixed = content.clone();
        fixed.replace_range(sug.span.byte_start as usize..sug.span.byte_end as usize, "");
        assert_eq!(fixed, "pub fn f() {}\n");
    }

    #[test]
    fn deletes_rust_comment_directive_line() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "src/lib.rs",
            "// workspace-lint: expect(unused-pub)\npub fn f() {}\n",
        );
        let sug = deletion_suggestion(tmp.path(), &origin("src/lib.rs", 1, 1)).unwrap();
        assert_eq!(
            sug.original.as_deref(),
            Some("// workspace-lint: expect(unused-pub)")
        );
    }

    #[test]
    fn deletes_multiline_macro_invocation() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "src/lib.rs",
            "workspace_lint::expect!(\n    unused_pub,\n);\npub fn f() {}\n",
        );
        // The macro invocation spans lines 1..=3.
        let sug = deletion_suggestion(tmp.path(), &origin("src/lib.rs", 1, 3)).unwrap();
        let content = std::fs::read_to_string(tmp.path().join("src/lib.rs")).unwrap();
        let mut fixed = content.clone();
        fixed.replace_range(sug.span.byte_start as usize..sug.span.byte_end as usize, "");
        assert_eq!(fixed, "pub fn f() {}\n");
    }

    // --- deletion_suggestion: guard rejections → None (help-only) ---

    #[test]
    fn rejects_code_sharing_the_macro_line() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "src/lib.rs",
            "workspace_lint::expect!(unused_pub); struct S;\n",
        );
        assert!(deletion_suggestion(tmp.path(), &origin("src/lib.rs", 1, 1)).is_none());
    }

    #[test]
    fn rejects_unterminated_html_comment_opener() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "README.md",
            "<!-- workspace-lint: expect(freshness)\n     still inside the comment -->\n",
        );
        assert!(deletion_suggestion(tmp.path(), &origin("README.md", 1, 1)).is_none());
    }

    #[test]
    fn accepts_self_contained_html_comment() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "README.md",
            "<!-- workspace-lint: expect(freshness) -->\n",
        );
        assert!(deletion_suggestion(tmp.path(), &origin("README.md", 1, 1)).is_some());
    }

    #[test]
    fn missing_file_is_none() {
        let tmp = TempDir::new().unwrap();
        assert!(deletion_suggestion(tmp.path(), &origin("nope.rs", 1, 1)).is_none());
    }

    #[test]
    fn unknown_extension_is_none() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "notes.txt",
            "workspace-lint: expect(freshness)\n",
        );
        assert!(deletion_suggestion(tmp.path(), &origin("notes.txt", 1, 1)).is_none());
    }
}
