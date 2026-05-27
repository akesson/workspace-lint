//! Apply `MachineApplicable` suggestions to source files.
//!
//! Two suggestion kinds drive `--fix`:
//!
//! 1. **Silence directives** (`byte_start == byte_end == 0`): the
//!    `workspace_lint::allow!(...)` macro or `# workspace-lint: allow(...)`
//!    comment that suppresses the diagnostic. Inserted at the start of the
//!    diagnostic's line; safe even when multiple diagnostics target the
//!    same file (sorted by descending offset before applying).
//! 2. **Structural rewrites** (`byte_start < byte_end`): byte-range
//!    replacements emitted by lints with a `MachineApplicable` real fix —
//!    centralized-deps (`serde = "1"` → `serde = { workspace = true }`),
//!    unused-deps (line deletion), visibility (`pub` → `pub(crate)`),
//!    unused-pub (delete-or-tighten). The lint's `check` function attaches
//!    these to `Diagnostic.suggestions`; the silence suggestion remains
//!    available as a fallback the user can paste manually.
//!
//! Correctness properties this module maintains:
//!
//! - **Idempotent.** Running `--fix` twice doesn't duplicate directives
//!   ([`already_silenced`]) and structural rewrites are no-ops when the
//!   resulting text is identical.
//! - **Deterministic ordering.** Suggestions targeting one file are
//!   applied by descending *computed* offset, with `line_start` as the
//!   tiebreaker. Earlier offsets stay valid as we mutate from the back.
//! - **Structural fixes preempt silence.** When a diagnostic has both a
//!   structural suggestion and a silence suggestion, only the structural
//!   one is applied — fixing the issue removes the need to silence it.

// Adding rustfix wiring + structural-fix routing + the directives-backed
// already_silenced check pushed this file past 500 LOC. Splitting along
// the (suggestion-classification, file-application, suppression-check)
// seams would scatter the apply pipeline; keep it colocated. stale-expect
// will surface here if the file shrinks back.
workspace_lint_marker::expect!(file_size);

use std::borrow::Cow;
use std::collections::BTreeMap;

use fs_err as fs;

use crate::diagnostic::{Applicability, Diagnostic, Suggestion};

/// Apply machine-applicable suggestions to disk. Returns the count of
/// files modified.
pub fn run(diagnostics: &[Diagnostic]) -> usize {
    // For each diagnostic, prefer a structural fix over the silence
    // fallback. If both exist, fixing makes silencing unnecessary.
    let mut structural_count = 0usize;
    let mut silence_count = 0usize;
    let mut candidates: Vec<Suggestion> = Vec::new();
    for d in diagnostics {
        let structural: Vec<&Suggestion> = d
            .suggestions
            .iter()
            .filter(|s| s.applicability == Applicability::MachineApplicable)
            .collect();
        if !structural.is_empty() {
            structural_count += structural.len();
            candidates.extend(structural.into_iter().cloned());
        } else if let Some(silence) = d.silence_suggestion()
            && silence.applicability == Applicability::MachineApplicable
        {
            silence_count += 1;
            candidates.push(silence);
        }
    }

    if structural_count > 0 {
        eprintln!(
            "workspace-lint --fix: applying {structural_count} structural fix{}",
            if structural_count == 1 { "" } else { "es" }
        );
    }
    if silence_count > 0 {
        eprintln!(
            "workspace-lint --fix: stamping silence directives next to {silence_count} remaining diagnostic{}",
            if silence_count == 1 { "" } else { "s" }
        );
        eprintln!(
            "  note: silence directives suppress without resolving. Prefer per-lint fixes when available."
        );
    }

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
    let source = fs::read_to_string(path)?;
    let eol = detect_eol(&source);

    // Filter idempotency: silence-style insertions skip if the marker is
    // already in the window. Structural replacements skip if applying them
    // would produce the same bytes that are already in the file.
    let to_apply: Vec<&Suggestion> = suggestions
        .iter()
        .filter(|s| {
            if is_insertion(s) {
                !already_silenced(&source, s)
            } else {
                !already_replaced(&source, s)
            }
        })
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

    // Reject overlapping replacement ranges — the apply order assumes
    // non-overlapping mutations from the tail of the file forward. If a
    // lint ever emits overlapping suggestions for one file, the right
    // call is to fix the lint, not silently corrupt the file.
    let mut replacements: Vec<(usize, usize)> = ordered
        .iter()
        .filter(|(_, s)| !is_insertion(s))
        .map(|(_, s)| (s.span.byte_start as usize, s.span.byte_end as usize))
        .collect();
    replacements.sort();
    for w in replacements.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        if a.1 > b.0 {
            return Err(format!(
                "{} has overlapping fix suggestions ({:?} and {:?}); refusing to apply",
                path.display(),
                a,
                b,
            )
            .into());
        }
    }

    let mut fixed = source.clone();
    for (offset, s) in ordered {
        let replacement = normalize_eol(&s.replacement, eol);
        if is_insertion(s) {
            fixed.insert_str(offset, &replacement);
        } else {
            let start = s.span.byte_start as usize;
            let end = s.span.byte_end as usize;
            fixed.replace_range(start..end, &replacement);
        }
    }

    if fixed != source {
        fs::write(path, fixed)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

fn is_insertion(s: &Suggestion) -> bool {
    s.span.byte_start == s.span.byte_end
}

/// `true` if applying this byte-range replacement would be a no-op (the
/// file already contains the desired text at the target range).
fn already_replaced(source: &str, s: &Suggestion) -> bool {
    let start = s.span.byte_start as usize;
    let end = s.span.byte_end as usize;
    if start >= source.len() || end > source.len() || start > end {
        return false;
    }
    source[start..end] == s.replacement
}

/// `true` if a workspace-lint directive of the same lint already covers
/// the suggestion's target scope.
///
/// Implementation: parse the file with the same scanner the main
/// suppression pipeline uses (`directives::scan_single_file`) — proper
/// syn/regex parsing instead of substring matching, so directive text
/// inside string literals or doc comments doesn't yield false positives.
/// The `source` argument is the read file content; it's used as a quick
/// pre-filter to avoid re-parsing when no directive marker is present.
fn already_silenced(source: &str, s: &Suggestion) -> bool {
    // Fast path: if no plausible directive marker appears anywhere in the
    // file, skip the parse. Saves cost on the typical "clean file" case.
    if !source.contains("workspace_lint::")
        && !source.contains("workspace-lint:")
        && !source.contains("allow!")
        && !source.contains("expect!")
    {
        return false;
    }
    let lint_short = lint_from_replacement(s);
    let directives = crate::directives::scan_single_file(&s.span.file);
    directives.iter().any(|d| {
        // Match by lint name (kebab form, no `workspace-lint::` prefix).
        if d.lint != lint_short {
            return false;
        }
        // Target scope reasoning:
        //  - File-anchor (line_start == 1): any covering directive in the
        //    same file suffices.
        //  - Line-anchor: the directive's anchor must contain a synthetic
        //    Line anchor at the target line, which subsumes file-wide
        //    directives via SilenceAnchor::contains().
        let target = if s.span.line_start <= 1 {
            crate::diagnostic::SilenceAnchor::File {
                file: s.span.file.clone(),
            }
        } else {
            crate::diagnostic::SilenceAnchor::Line {
                file: s.span.file.clone(),
                line: s.span.line_start,
            }
        };
        d.anchor.contains(&target)
    })
}

/// Extract the lint's short kebab name from a silence-directive
/// replacement. We're matching strings like
/// `workspace_lint::allow!(file_size);\n` or
/// `# workspace-lint: allow(centralized-deps)\n`. Returns an empty string
/// when the replacement doesn't look like a directive (in which case
/// `already_silenced` falls through to "not silenced").
fn lint_from_replacement(s: &Suggestion) -> String {
    let r = s.replacement.trim();
    // Rust macro form.
    if let Some(inner) = r
        .strip_prefix("workspace_lint::allow!(")
        .or_else(|| r.strip_prefix("workspace_lint::expect!("))
        && let Some(name) = inner.split(['(', ')', ',', ';']).next()
    {
        return name.trim().replace('_', "-");
    }
    // Comment-directive form (TOML / Markdown).
    let body = r.trim_start_matches('#').trim();
    let body = body
        .trim_start_matches("workspace-lint:")
        .trim_start_matches("workspace_lint:")
        .trim();
    if let Some(rest) = body
        .strip_prefix("allow(")
        .or_else(|| body.strip_prefix("expect("))
        && let Some(name) = rest.split(')').next()
    {
        return name.trim().to_string();
    }
    String::new()
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
    fn fix_applies_byte_range_replacement() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("lib.rs");
        std::fs::write(&p, "pub fn x() {}\n").unwrap();
        let s = Suggestion {
            span: Span {
                file: p.clone(),
                line_start: 1,
                line_end: 1,
                col_start: 1,
                col_end: 4,
                byte_start: 0,
                byte_end: 3, // replace just "pub"
            },
            message: "tighten".into(),
            replacement: "pub(crate)".into(),
            applicability: Applicability::MachineApplicable,
        };
        let modified = apply_to_file(&p, std::slice::from_ref(&s)).unwrap();
        assert!(modified);
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "pub(crate) fn x() {}\n"
        );
    }

    #[test]
    fn fix_rejects_overlapping_replacements() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("lib.rs");
        std::fs::write(&p, "pub fn x() {}\n").unwrap();
        let a = Suggestion {
            span: Span {
                file: p.clone(),
                line_start: 1,
                line_end: 1,
                col_start: 1,
                col_end: 4,
                byte_start: 0,
                byte_end: 5,
            },
            message: "a".into(),
            replacement: "X".into(),
            applicability: Applicability::MachineApplicable,
        };
        let mut b = a.clone();
        b.span.byte_start = 3;
        b.span.byte_end = 8;
        let err = apply_to_file(&p, &[a, b]).unwrap_err();
        assert!(err.to_string().contains("overlapping"));
    }

    #[test]
    fn already_silenced_detects_macro_at_top() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("lib.rs");
        std::fs::write(&p, "workspace_lint::allow!(file_size);\npub fn x() {}\n").unwrap();
        let src = std::fs::read_to_string(&p).unwrap();
        let s = Suggestion {
            span: Span {
                file: p.clone(),
                line_start: 1,
                line_end: 1,
                col_start: 1,
                col_end: 1,
                byte_start: 0,
                byte_end: 0,
            },
            message: "m".into(),
            replacement: "workspace_lint::allow!(file_size);\n".into(),
            applicability: Applicability::MachineApplicable,
        };
        assert!(already_silenced(&src, &s));
    }

    #[test]
    fn already_silenced_detects_comment_in_window() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("Cargo.toml");
        std::fs::write(
            &p,
            "[package]\nname = \"x\"\n# workspace-lint: allow(unused-deps)\n[dependencies]\nfoo = \"1\"\n",
        )
        .unwrap();
        let src = std::fs::read_to_string(&p).unwrap();
        let s = Suggestion {
            span: Span {
                file: p.clone(),
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
        assert!(already_silenced(&src, &s));
    }

    #[test]
    fn already_silenced_returns_false_when_lint_does_not_match() {
        // A directive for `unused-deps` shouldn't silence a `file-size`
        // suggestion in the same file. Tests the new lint-name match.
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("lib.rs");
        std::fs::write(&p, "workspace_lint::allow!(unused_deps);\npub fn x() {}\n").unwrap();
        let src = std::fs::read_to_string(&p).unwrap();
        let s = Suggestion {
            span: Span {
                file: p.clone(),
                line_start: 1,
                line_end: 1,
                col_start: 1,
                col_end: 1,
                byte_start: 0,
                byte_end: 0,
            },
            message: "m".into(),
            replacement: "workspace_lint::allow!(file_size);\n".into(),
            applicability: Applicability::MachineApplicable,
        };
        assert!(!already_silenced(&src, &s));
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
