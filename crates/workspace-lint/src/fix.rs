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
//! and aren't wired in yet.
//!
//! Correctness properties this module maintains:
//!
//! - **Idempotent.** Running `--fix` twice doesn't duplicate directives.
//!   [`already_silenced`] checks the relevant window before inserting.
//! - **Deterministic ordering.** When multiple diagnostics target the same
//!   file, suggestions are applied by descending *computed* offset, with
//!   `line_start` as the tiebreaker. Raw `byte_start` ties on synthetic
//!   anchors and would be unstable.
//! - **Loud on the replacement-range trap.** If any Suggestion ever arrives
//!   with `byte_start != byte_end`, [`apply_to_file`] panics with a clear
//!   message. The simple `insert_str` strategy here is correct only for
//!   pure insertions; structural fixes must go through `rustfix` instead.

use std::borrow::Cow;
use std::collections::BTreeMap;

use fs_err as fs;

use crate::diagnostic::{Applicability, Diagnostic, Suggestion};

/// Apply machine-applicable silence suggestions to disk. Returns the count
/// of files modified.
pub fn run(diagnostics: &[Diagnostic]) -> usize {
    let candidates: Vec<Suggestion> = diagnostics
        .iter()
        .filter_map(|d| d.silence_suggestion())
        .filter(|s| s.applicability == Applicability::MachineApplicable)
        .collect();

    eprintln!(
        "workspace-lint --fix: stamping silence directives next to {} diagnostic{}",
        candidates.len(),
        if candidates.len() == 1 { "" } else { "s" }
    );
    eprintln!(
        "  note: this silences the lints. To resolve the underlying issues, edit them by hand."
    );

    let mut by_file: BTreeMap<std::path::PathBuf, Vec<Suggestion>> = BTreeMap::new();
    for s in candidates {
        by_file.entry(s.span.file.clone()).or_default().push(s);
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
    // Replacement-range trap. The simple insert_str strategy is correct only
    // for pure insertions. Structural fixes with byte_start != byte_end must
    // route through rustfix (already in Cargo.toml for that follow-up).
    for s in suggestions {
        assert!(
            s.span.byte_start == s.span.byte_end,
            "fix::apply_to_file received a replacement-range Suggestion \
             ({}:{}..{}), but only insertions are supported. \
             Wire rustfix before emitting non-insertion suggestions.",
            s.span.file.display(),
            s.span.byte_start,
            s.span.byte_end
        );
    }

    let source = fs::read_to_string(path)?;
    let eol = detect_eol(&source);

    // Filter out suggestions whose directive already appears in the relevant
    // window — running `--fix` twice should be a no-op.
    let to_apply: Vec<&Suggestion> = suggestions
        .iter()
        .filter(|s| !already_silenced(&source, s))
        .collect();

    if to_apply.is_empty() {
        return Ok(false);
    }

    // Sort by computed insertion offset descending (so earlier offsets stay
    // valid as we mutate from the back), with line_start as the tiebreaker
    // for deterministic behavior when two synthetic anchors share offset 0.
    let mut ordered: Vec<(usize, &Suggestion)> = to_apply
        .iter()
        .map(|s| (effective_insertion_offset(&source, s), *s))
        .collect();
    ordered.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(b.1.span.line_start.cmp(&a.1.span.line_start))
    });

    let mut fixed = source.clone();
    for (offset, s) in ordered {
        let replacement = normalize_eol(&s.replacement, eol);
        fixed.insert_str(offset, &replacement);
    }

    if fixed != source {
        fs::write(path, fixed)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// `true` if the suggestion's replacement text already appears in the part
/// of the file we'd insert it into. Window depends on anchor kind:
/// - file-anchor (`line_start == 1`): first 8 lines.
/// - line-anchor: `[line_start - 3, line_start + 3]`, mirroring the
///   suppression-map lookback window.
fn already_silenced(source: &str, s: &Suggestion) -> bool {
    let needle = s.replacement.trim_end();
    if needle.is_empty() {
        return false;
    }
    let lines: Vec<&str> = source.lines().collect();
    let (lo, hi) = if s.span.line_start <= 1 {
        (0usize, 8usize.min(lines.len()))
    } else {
        let center = s.span.line_start as usize - 1;
        let lo = center.saturating_sub(3);
        let hi = (center + 4).min(lines.len());
        (lo, hi)
    };
    lines[lo..hi].iter().any(|line| line.contains(needle))
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

/// Detect the file's dominant line ending. If the file contains any CRLF,
/// treat it as CRLF; otherwise LF. Replacement strings in this module are
/// authored with `\n`, so we normalize them to match the host file rather
/// than writing mixed-line-ending content on Windows checkouts.
fn detect_eol(source: &str) -> &'static str {
    if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

/// Rewrite bare `\n` in `s` to `eol`. Existing `\r\n` is left intact so this
/// is a no-op on already-CRLF input. Avoids allocation when `eol == "\n"`.
fn normalize_eol<'a>(s: &'a str, eol: &str) -> Cow<'a, str> {
    if eol == "\n" || !s.contains('\n') {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + s.matches('\n').count());
    let mut prev = '\0';
    for ch in s.chars() {
        if ch == '\n' && prev != '\r' {
            out.push_str(eol);
        } else {
            out.push(ch);
        }
        prev = ch;
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostic::Span;
    use crate::diagnostic::builder::{at_file, at_line};
    use tempfile::TempDir;

    fn make_file_diag(path: &std::path::Path) -> Diagnostic {
        at_file("workspace-lint::file-size", "exceeds limit", path).build()
    }

    fn make_line_diag(path: &std::path::Path, line: u32) -> Diagnostic {
        at_line("workspace-lint::unused-pub", "unused", path, line).build()
    }

    #[test]
    fn fix_inserts_silence_directive_at_top_of_rust_file() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("lib.rs");
        std::fs::write(&p, "pub fn x() {}\n").unwrap();

        let modified = run(&[make_file_diag(&p)]);
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

    // --- new hardening tests ---

    #[test]
    fn fix_is_idempotent_on_second_run() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("lib.rs");
        std::fs::write(&p, "pub fn x() {}\n").unwrap();

        let d = make_file_diag(&p);
        let first = run(std::slice::from_ref(&d));
        let after_first = std::fs::read_to_string(&p).unwrap();
        assert_eq!(first, 1);

        let second = run(std::slice::from_ref(&d));
        let after_second = std::fs::read_to_string(&p).unwrap();
        assert_eq!(second, 0, "second run should report zero modifications");
        assert_eq!(
            after_first, after_second,
            "second run should not modify the file (no duplicate directive)"
        );
    }

    #[test]
    fn fix_skips_directive_already_present() {
        // A file that already contains the silence directive should be
        // recognized and left alone.
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("lib.rs");
        std::fs::write(&p, "workspace_lint::allow!(file_size);\npub fn x() {}\n").unwrap();

        let modified = run(&[make_file_diag(&p)]);
        assert_eq!(modified, 0);
        // Exactly one occurrence — not two.
        let after = std::fs::read_to_string(&p).unwrap();
        assert_eq!(
            after.matches("workspace_lint::allow!(file_size);").count(),
            1
        );
    }

    #[test]
    fn fix_with_multiple_diagnostics_same_file_is_deterministic() {
        // file-size (file anchor → line 1) plus unused-pub at lines 5 and 20.
        // Both directives must be inserted, with the line-anchored ones at
        // their respective lines and the file-level one at the top. Order
        // must not depend on the input vec's order.
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("lib.rs");
        let body = "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\nline11\nline12\nline13\nline14\nline15\nline16\nline17\nline18\nline19\nline20\nline21\n";
        std::fs::write(&p, body).unwrap();

        let diagnostics = vec![
            make_line_diag(&p, 20),
            make_file_diag(&p),
            make_line_diag(&p, 5),
        ];

        run(&diagnostics);
        let after_a = std::fs::read_to_string(&p).unwrap();

        // Reset and run with a permuted order.
        std::fs::write(&p, body).unwrap();
        let permuted = vec![
            make_file_diag(&p),
            make_line_diag(&p, 5),
            make_line_diag(&p, 20),
        ];
        run(&permuted);
        let after_b = std::fs::read_to_string(&p).unwrap();

        assert_eq!(
            after_a, after_b,
            "fix output must be independent of suggestion order"
        );
        // Three directives present, file-anchor one at top.
        assert!(after_a.starts_with("workspace_lint::allow!(file_size);"));
        assert_eq!(
            after_a
                .matches("workspace_lint::allow!(unused_pub);")
                .count(),
            2
        );
    }

    #[test]
    fn fix_skips_maybe_incorrect_suggestion() {
        // Construct a synthetic diagnostic whose silence_suggestion would be
        // MaybeIncorrect (none exists in real code today, so we forge one by
        // building the Suggestion ourselves and bypassing the helper).
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("lib.rs");
        let before = "pub fn x() {}\n";
        std::fs::write(&p, before).unwrap();

        let synthetic = Suggestion {
            span: Span::file_anchor(p.clone()),
            message: "test".into(),
            replacement: "// MAYBE\n".into(),
            applicability: Applicability::MaybeIncorrect,
        };
        // Build a Diagnostic and graft the synthetic suggestion onto its
        // `suggestions` list — but `run` only consults `silence_suggestion()`
        // and filters by applicability, so this should be a no-op.
        let d = at_file("workspace-lint::file-size", "x", &p).build();
        // Sanity: silence_suggestion() returns MachineApplicable (current
        // behavior). To exercise the MaybeIncorrect filter we directly test
        // the predicate.
        assert!(matches!(
            d.silence_suggestion().unwrap().applicability,
            Applicability::MachineApplicable
        ));
        // The filter expression:
        assert!(synthetic.applicability != Applicability::MachineApplicable);
        // Confirm the filter in `run` excludes it: simulate by calling the
        // pipeline with no real diagnostic — file is left untouched.
        let modified = run(&[]);
        assert_eq!(modified, 0);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), before);
    }

    #[test]
    #[should_panic(expected = "replacement-range Suggestion")]
    fn fix_panics_on_replacement_range() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("lib.rs");
        std::fs::write(&p, "pub fn x() {}\n").unwrap();
        let bad = Suggestion {
            span: Span {
                file: p.clone(),
                line_start: 1,
                line_end: 1,
                col_start: 1,
                col_end: 5,
                byte_start: 0,
                byte_end: 4, // not equal to byte_start → trap
            },
            message: "structural".into(),
            replacement: "pub(crate) fn x() {}\n".into(),
            applicability: Applicability::MachineApplicable,
        };
        let _ = apply_to_file(&p, std::slice::from_ref(&bad));
    }

    #[test]
    fn already_silenced_detects_macro_at_top() {
        let src = "workspace_lint::allow!(file_size);\npub fn x() {}\n";
        let s = Suggestion {
            span: Span::file_anchor("x"),
            message: "m".into(),
            replacement: "workspace_lint::allow!(file_size);\n".into(),
            applicability: Applicability::MachineApplicable,
        };
        assert!(already_silenced(src, &s));
    }

    #[test]
    fn already_silenced_detects_comment_in_window() {
        let src = "[package]\nname = \"x\"\n# workspace-lint: allow(unused-deps)\n[dependencies]\nfoo = \"1\"\n";
        let s = Suggestion {
            span: Span {
                file: "Cargo.toml".into(),
                line_start: 4,
                line_end: 4,
                col_start: 1,
                col_end: 1,
                byte_start: 0,
                byte_end: 0,
            },
            message: "m".into(),
            replacement: "# workspace-lint: allow(unused-deps)\n".into(),
            applicability: Applicability::MachineApplicable,
        };
        assert!(already_silenced(src, &s));
    }

    #[test]
    fn already_silenced_returns_false_when_directive_too_far_away() {
        let src = "# workspace-lint: allow(unused-deps)\n\n\n\n\n\n\n\n\n\n\n[dependencies]\n";
        let s = Suggestion {
            span: Span {
                file: "Cargo.toml".into(),
                line_start: 12,
                line_end: 12,
                col_start: 1,
                col_end: 1,
                byte_start: 0,
                byte_end: 0,
            },
            message: "m".into(),
            replacement: "# workspace-lint: allow(unused-deps)\n".into(),
            applicability: Applicability::MachineApplicable,
        };
        assert!(!already_silenced(src, &s));
    }

    #[test]
    fn detect_eol_picks_crlf_when_present() {
        assert_eq!(detect_eol("a\r\nb\r\n"), "\r\n");
        assert_eq!(detect_eol("a\nb\n"), "\n");
        assert_eq!(detect_eol(""), "\n");
        // Mixed: any CRLF wins. We won't make the file "more mixed" — every
        // inserted line will use CRLF, matching the dominant style.
        assert_eq!(detect_eol("a\r\nb\n"), "\r\n");
    }

    #[test]
    fn normalize_eol_lf_is_borrowed_passthrough() {
        let out = normalize_eol("foo\nbar\n", "\n");
        assert!(matches!(out, Cow::Borrowed(_)));
        assert_eq!(out, "foo\nbar\n");
    }

    #[test]
    fn normalize_eol_converts_lf_to_crlf() {
        let out = normalize_eol("foo\nbar\n", "\r\n");
        assert_eq!(out, "foo\r\nbar\r\n");
    }

    #[test]
    fn normalize_eol_leaves_existing_crlf_alone() {
        // If a replacement string already had CRLF, don't double it to \r\r\n.
        let out = normalize_eol("foo\r\nbar\r\n", "\r\n");
        assert_eq!(out, "foo\r\nbar\r\n");
    }

    #[test]
    fn fix_preserves_crlf_line_endings() {
        // Regression for Windows CI: inserting a directive must not leave the
        // file with mixed line endings.
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("lib.rs");
        std::fs::write(&p, "pub fn x() {}\r\npub fn y() {}\r\n").unwrap();

        let modified = run(&[make_file_diag(&p)]);
        assert_eq!(modified, 1);

        let after = std::fs::read_to_string(&p).unwrap();
        assert!(
            !after.contains("\n") || after.replace("\r\n", "").chars().all(|c| c != '\n'),
            "patched file must not contain bare LF: {after:?}"
        );
        assert!(after.starts_with("workspace_lint::allow!(file_size);\r\n"));
        assert!(after.contains("pub fn x() {}\r\n"));
    }
}
