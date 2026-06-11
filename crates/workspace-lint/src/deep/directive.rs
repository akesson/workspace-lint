//! Build the `expect` directive insertion `--fix` writes above a finding that
//! rust-analyzer disproved. The directive is a zero-width insertion (it adds a
//! line, replaces nothing) carrying provenance so a human reviewing the
//! `git diff` sees *why* it appeared.

use std::path::Path;

use fs_err as fs;

use crate::diagnostic::{Applicability, Span, Suggestion};

/// Build a zero-width [`Suggestion`] inserting
/// `<indent><marker> workspace-lint: expect(<lint>) -- <provenance>` on its own
/// line, immediately above 1-based `line` in `file`, matching that line's
/// indentation. `marker` is `//` for Rust sources, `#` for TOML — picked from
/// the file extension. The trailing provenance is free text the directive
/// regex ignores (it is not `)`-anchored).
///
/// Returns `None` when the file can't be read, the line is out of range, or an
/// identical `expect(<lint>)` directive already sits within the three lines
/// above (the suppression lookback window) — so a re-run doesn't stack
/// duplicates.
pub(crate) fn build_expect_insert(
    file: &Path,
    line: u32,
    lint: &str,
    provenance: &str,
) -> Option<Suggestion> {
    let source = fs::read_to_string(file).ok()?;
    let (line_start_byte, indent) = line_start_and_indent(&source, line)?;
    if directive_already_present(&source, line, lint) {
        return None;
    }
    let marker = if file.extension().and_then(|e| e.to_str()) == Some("rs") {
        "//"
    } else {
        "#"
    };
    let replacement = format!("{indent}{marker} workspace-lint: expect({lint}) -- {provenance}\n");
    Some(Suggestion {
        span: Span {
            file: file.to_path_buf(),
            line_start: line,
            line_end: line,
            col_start: 1,
            col_end: 1,
            byte_start: line_start_byte,
            byte_end: line_start_byte, // zero-width = insertion
        },
        message: format!("write `expect({lint})` (rust-analyzer disproved this finding)"),
        replacement,
        applicability: Applicability::MachineApplicable,
        evidence: None,
    })
}

/// Byte offset where 1-based `line` begins, and that line's leading-whitespace
/// indent. `None` if `line` is past the end of the file.
fn line_start_and_indent(source: &str, line: u32) -> Option<(u32, String)> {
    let mut offset = 0usize;
    for (idx, l) in source.split_inclusive('\n').enumerate() {
        if idx as u32 + 1 == line {
            let indent: String = l.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
            return Some((offset as u32, indent));
        }
        offset += l.len();
    }
    None
}

/// `true` if an identical `expect(<lint>)` directive already sits on one of the
/// lines immediately above `line`, within the same [`crate::suppress::LOOKBACK_FORWARD`]
/// window the suppressor honors — so writing another would be redundant.
fn directive_already_present(source: &str, line: u32, lint: &str) -> bool {
    let needle = format!("expect({lint})");
    let lines: Vec<&str> = source.lines().collect();
    // The LOOKBACK_FORWARD lines above `line` (1-based): the suppressor binds a
    // directive to a diagnostic up to that many lines below it, so a directive
    // within that window above the item already covers it.
    let above_end = (line as usize).saturating_sub(1); // 0-based index of `line`'s predecessor + 1
    let above_start = above_end.saturating_sub(crate::suppress::LOOKBACK_FORWARD as usize);
    lines
        .get(above_start..above_end)
        .into_iter()
        .flatten()
        .any(|l| l.contains("workspace-lint:") && l.contains(&needle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(name: &str, content: &str) -> (TempDir, std::path::PathBuf) {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join(name);
        std::fs::write(&p, content).unwrap();
        (tmp, p)
    }

    #[test]
    fn inserts_rust_comment_above_item_with_indent() {
        let (_t, p) = write("lib.rs", "mod m {\n    pub fn helper() {}\n}\n");
        let s = build_expect_insert(&p, 2, "unused-pub", "rust-analyzer sees it (x.rs:5)").unwrap();
        assert_eq!(
            s.replacement,
            "    // workspace-lint: expect(unused-pub) -- rust-analyzer sees it (x.rs:5)\n"
        );
        // Zero-width insertion at the start of line 2 (after `mod m {\n` = 8 bytes).
        assert_eq!(s.span.byte_start, 8);
        assert_eq!(s.span.byte_end, 8);
    }

    #[test]
    fn inserts_toml_hash_comment() {
        let (_t, p) = write("Cargo.toml", "[dependencies]\nstrum = \"0.26\"\n");
        let s = build_expect_insert(&p, 2, "unused-deps", "sees strum (src/lib.rs:1)").unwrap();
        assert!(
            s.replacement
                .starts_with("# workspace-lint: expect(unused-deps) -- ")
        );
        assert_eq!(s.span.byte_start, 15); // after "[dependencies]\n"
    }

    #[test]
    fn skips_when_directive_already_present() {
        let (_t, p) = write(
            "lib.rs",
            "// workspace-lint: expect(unused-pub) -- old\npub fn helper() {}\n",
        );
        assert!(
            build_expect_insert(&p, 2, "unused-pub", "new").is_none(),
            "must not stack a duplicate directive"
        );
    }

    #[test]
    fn allows_when_different_lint_present() {
        let (_t, p) = write(
            "lib.rs",
            "// workspace-lint: expect(file-size) -- x\npub fn helper() {}\n",
        );
        assert!(
            build_expect_insert(&p, 2, "unused-pub", "new").is_some(),
            "a directive for a different lint must not block this one"
        );
    }

    #[test]
    fn none_for_line_past_eof() {
        let (_t, p) = write("lib.rs", "pub fn a() {}\n");
        assert!(build_expect_insert(&p, 99, "unused-pub", "x").is_none());
    }
}
