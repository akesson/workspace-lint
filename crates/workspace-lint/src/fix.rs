//! Apply `MachineApplicable` suggestions to source files.
//!
//! Currently the only `MachineApplicable` suggestion every diagnostic carries
//! is the *silence* directive — the `workspace_lint::allow!(...)` macro or
//! `# workspace-lint: allow(...)` comment that suppresses the diagnostic.
//! Running `workspace-lint --fix` therefore stamps a silence directive next
//! to every diagnostic that fired.
//!
//! This is intentional and dangerous: it doesn't *resolve* the underlying
//! issues, it silences them. Per-lint structural fixes (rewriting
//! `serde = "1"` → `serde = { workspace = true }`, deleting unused dep
//! lines, tightening `pub` → `pub(crate)`) belong on top of this scaffold
//! and aren't wired in yet. For now `--fix` prints a clear stderr warning
//! describing what it will do before it does it.
//!
//! Today the only suggestions are insertions (silence directives), so the
//! application loop is a simple reverse-sorted insert. When structural
//! fixes land they will hand work off to the `rustfix` crate (already in
//! [`Cargo.toml`]) for overlap detection.

use std::collections::BTreeMap;

use fs_err as fs;

use crate::diagnostic::{Applicability, Diagnostic, Suggestion};

/// Apply machine-applicable silence suggestions to disk. Returns the count
/// of files modified.
pub fn run(diagnostics: &[Diagnostic]) -> usize {
    eprintln!(
        "workspace-lint --fix: stamping silence directives next to {} diagnostic{}",
        diagnostics.len(),
        if diagnostics.len() == 1 { "" } else { "s" }
    );
    eprintln!(
        "  note: this silences the lints. To resolve the underlying issues, edit them by hand."
    );

    // Group suggestions by file so each file is opened/written once.
    let mut by_file: BTreeMap<std::path::PathBuf, Vec<Suggestion>> = BTreeMap::new();
    for d in diagnostics {
        if let Some(s) = d.silence_suggestion()
            && s.applicability == Applicability::MachineApplicable
        {
            by_file.entry(s.span.file.clone()).or_default().push(s);
        }
    }

    let mut modified_count = 0;
    for (path, suggestions) in by_file {
        match apply_to_file(&path, &suggestions) {
            Ok(true) => modified_count += 1,
            Ok(false) => {}
            Err(e) => {
                eprintln!("warning: failed to fix {}: {e}", path.display());
            }
        }
    }

    if modified_count > 0 {
        eprintln!(
            "workspace-lint --fix: modified {modified_count} file{}",
            if modified_count == 1 { "" } else { "s" }
        );
    } else {
        eprintln!("workspace-lint --fix: no files modified");
    }
    modified_count
}

fn apply_to_file(
    path: &std::path::Path,
    suggestions: &[Suggestion],
) -> Result<bool, Box<dyn std::error::Error>> {
    let source = fs::read_to_string(path)?;
    let mut fixed = source.clone();

    // We currently only emit *insertion* suggestions (silence directives), so
    // the byte_start == byte_end for every Suggestion. Applying them is
    // simple: insert at the right offset, working back-to-front so earlier
    // offsets stay valid.
    let mut ordered = suggestions.to_vec();
    ordered.sort_by_key(|s| std::cmp::Reverse(s.span.byte_start));

    for s in ordered {
        let pos = effective_insertion_offset(&fixed, &s);
        fixed.insert_str(pos, &s.replacement);
    }

    if fixed != source {
        fs::write(path, fixed)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Pick where to insert the directive in the source. The Suggestion carries
/// `byte_start`/`byte_end` (both 0 for synthetic anchors), so for a real
/// fix we want to find the start of the diagnostic's line and insert just
/// before it. Use `line_start` to do that line-by-line.
fn effective_insertion_offset(source: &str, s: &Suggestion) -> usize {
    if s.span.byte_start > 0 {
        return s.span.byte_start as usize;
    }
    let target_line = s.span.line_start.max(1) as usize;
    let mut current_line = 1usize;
    for (idx, ch) in source.char_indices() {
        if current_line == target_line {
            return idx;
        }
        if ch == '\n' {
            current_line += 1;
        }
    }
    source.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostic::Span;
    use crate::diagnostic::builder::at_file;
    use tempfile::TempDir;

    fn make_diag_at(path: &std::path::Path) -> Diagnostic {
        at_file("workspace-lint::file-size", "exceeds limit", path).build()
    }

    #[test]
    fn fix_inserts_silence_directive_at_top_of_rust_file() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("lib.rs");
        std::fs::write(&p, "pub fn x() {}\n").unwrap();

        let modified = run(&[make_diag_at(&p)]);
        assert_eq!(modified, 1);
        let after = std::fs::read_to_string(&p).unwrap();
        assert!(after.starts_with("workspace_lint::allow!(file_size);"));
        assert!(after.contains("pub fn x() {}"));
    }

    #[test]
    fn fix_idempotent_when_no_diagnostics() {
        let modified = run(&[]);
        assert_eq!(modified, 0);
    }

    #[test]
    fn fix_inserts_comment_directive_in_toml() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("Cargo.toml");
        std::fs::write(&p, "[package]\nname = \"x\"\n").unwrap();

        let d = at_file("workspace-lint::unused-deps", "x", &p).build();
        let modified = run(&[d]);
        assert_eq!(modified, 1);
        let after = std::fs::read_to_string(&p).unwrap();
        assert!(after.starts_with("# workspace-lint: allow(unused-deps)"));
    }

    #[test]
    fn insertion_offset_falls_back_to_target_line() {
        let src = "line one\nline two\nline three\n";
        let s = Suggestion {
            span: Span {
                file: "x".into(),
                line_start: 2,
                line_end: 2,
                col_start: 1,
                col_end: 1,
                byte_start: 0,
                byte_end: 0,
            },
            message: "m".into(),
            replacement: "INS\n".into(),
            applicability: Applicability::MachineApplicable,
        };
        // line 2 starts at byte 9 (after "line one\n").
        assert_eq!(effective_insertion_offset(src, &s), 9);
    }
}
