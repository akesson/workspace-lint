//! Apply `MachineApplicable` structural suggestions to source files.
//!
//! `--fix` only applies real per-lint rewrites — byte-range replacements
//! produced by lints that know how to resolve their own findings:
//! centralized-deps (`serde = "1"` → `serde = { workspace = true }`),
//! unused-deps (line deletion), visibility (`pub` → `pub(crate)`),
//! unused-pub (delete-or-tighten). The lint's `check` function attaches
//! these to `Diagnostic.suggestions` with `Applicability::MachineApplicable`.
//!
//! Diagnostics without a structural suggestion are left untouched. The
//! human/JSON/github renderers still print the diagnostic's "if intentional,
//! silence with:" hint for a human to paste — `--fix` will never edit a
//! file to suppress a diagnostic it didn't actually fix.
//!
//! Correctness properties this module maintains:
//!
//! - **Idempotent.** Re-running `--fix` on a clean tree is a no-op:
//!   `already_replaced` short-circuits suggestions whose target range
//!   already equals the desired text.
//! - **Deterministic ordering.** Suggestions targeting one file are
//!   applied by descending byte offset, with `line_start` as the tiebreaker.
//!   Earlier offsets stay valid as we mutate from the back.
//! - **No overlap.** Overlapping replacement spans abort the file rather
//!   than silently corrupting it.

use std::borrow::Cow;
use std::collections::BTreeMap;

use fs_err as fs;

use crate::diagnostic::{Applicability, Diagnostic, Suggestion};

/// Apply machine-applicable structural suggestions to disk. Returns the
/// count of files modified.
pub(crate) fn run(diagnostics: &[Diagnostic]) -> usize {
    let mut structural_count = 0usize;
    let mut candidates: Vec<Suggestion> = Vec::new();
    for d in diagnostics {
        for s in &d.suggestions {
            if s.applicability == Applicability::MachineApplicable
                && s.span.byte_end > s.span.byte_start
            {
                structural_count += 1;
                candidates.push(s.clone());
            }
        }
    }

    if structural_count == 0 {
        eprintln!("workspace-lint --fix: no structural fixes available");
        return 0;
    }

    eprintln!(
        "workspace-lint --fix: applying {structural_count} structural fix{}",
        if structural_count == 1 { "" } else { "es" }
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
    let source = fs::read_to_string(path)?;
    let eol = detect_eol(&source);

    let to_apply: Vec<&Suggestion> = suggestions
        .iter()
        .filter(|s| !already_replaced(&source, s))
        .collect();

    if to_apply.is_empty() {
        return Ok(false);
    }

    // Sort by byte offset descending so earlier offsets stay valid as we
    // mutate from the back. `line_start` is the tiebreaker for deterministic
    // behavior when two suggestions share a byte_start.
    let mut ordered: Vec<&Suggestion> = to_apply;
    ordered.sort_by(|a, b| {
        b.span
            .byte_start
            .cmp(&a.span.byte_start)
            .then(b.span.line_start.cmp(&a.span.line_start))
    });

    // Reject overlapping replacement ranges — the apply order assumes
    // non-overlapping mutations from the tail of the file forward. If a
    // lint ever emits overlapping suggestions for one file, the right
    // call is to fix the lint, not silently corrupt the file.
    let mut replacements: Vec<(usize, usize)> = ordered
        .iter()
        .map(|s| (s.span.byte_start as usize, s.span.byte_end as usize))
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
    for s in ordered {
        let replacement = normalize_eol(&s.replacement, eol);
        let start = s.span.byte_start as usize;
        let end = s.span.byte_end as usize;
        fixed.replace_range(start..end, &replacement);
    }

    if fixed != source {
        fs::write(path, fixed)?;
        Ok(true)
    } else {
        Ok(false)
    }
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
    use crate::diagnostic::{Diagnostic, Level, SilenceAnchor, Span};
    use std::borrow::Cow;
    use tempfile::TempDir;

    fn structural_diag(path: &std::path::Path, span: Span, replacement: &str) -> Diagnostic {
        Diagnostic {
            lint: Cow::Borrowed("workspace-lint::unused-pub"),
            level: Level::Warn,
            message: "test".into(),
            primary: Some(span.clone()),
            helps: vec![],
            notes: vec![],
            suggestions: vec![Suggestion {
                span,
                message: "tighten".into(),
                replacement: replacement.into(),
                applicability: Applicability::MachineApplicable,
            }],
            silence_anchor: SilenceAnchor::File {
                file: path.to_path_buf(),
            },
            level_is_explicit: false,
        }
    }

    #[test]
    fn fix_with_no_diagnostics_is_a_noop() {
        let modified = run(&[]);
        assert_eq!(modified, 0);
    }

    #[test]
    fn fix_leaves_files_alone_when_no_structural_suggestion_exists() {
        // A diagnostic with no suggestions in `Diagnostic.suggestions` must
        // not trigger any file mutation. This guards against regression to
        // silence-stamping behavior.
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("lib.rs");
        let original = "pub fn x() {}\n";
        std::fs::write(&p, original).unwrap();

        let d = Diagnostic {
            lint: Cow::Borrowed("workspace-lint::file-size"),
            level: Level::Warn,
            message: "file too big".into(),
            primary: Some(Span::file_anchor(p.clone())),
            helps: vec![],
            notes: vec![],
            suggestions: vec![],
            silence_anchor: SilenceAnchor::File { file: p.clone() },
            level_is_explicit: false,
        };

        let modified = run(&[d]);
        assert_eq!(modified, 0);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), original);
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
    fn fix_is_idempotent_when_replacement_already_equals_target() {
        // `already_replaced` short-circuits when the byte range already
        // contains the desired text — the case where a fix-equivalent
        // edit has been applied by hand. Real-world idempotency on
        // re-run additionally comes from the lint no longer firing once
        // the fix has been applied; that's tested at the fixture level.
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("lib.rs");
        std::fs::write(&p, "pub(crate) fn x() {}\n").unwrap();

        let span = Span {
            file: p.clone(),
            line_start: 1,
            line_end: 1,
            col_start: 1,
            col_end: 11,
            byte_start: 0,
            byte_end: 10,
        };
        let d = structural_diag(&p, span, "pub(crate)");
        let modified = run(std::slice::from_ref(&d));
        assert_eq!(modified, 0, "no-op fix should not touch the file");
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
    fn detect_eol_picks_crlf_when_present() {
        assert_eq!(detect_eol("a\r\nb\r\n"), "\r\n");
        assert_eq!(detect_eol("a\nb\n"), "\n");
        assert_eq!(detect_eol(""), "\n");
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
        let out = normalize_eol("foo\r\nbar\r\n", "\r\n");
        assert_eq!(out, "foo\r\nbar\r\n");
    }
}
