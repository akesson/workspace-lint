//! Apply `MachineApplicable` structural suggestions to source files.
//!
//! `--fix` applies real per-lint rewrites — byte-range replacements produced by
//! lints that know how to resolve their own findings: centralized-deps
//! (`serde = "1"` → `serde = { workspace = true }`), unused-deps (line
//! deletion), unused-pub (delete the item, or tighten `pub` → `pub(crate)`),
//! and stale-expect (delete the now-pointless directive line). The lint's
//! `check` function attaches these to `Diagnostic.suggestions` with
//! `Applicability::MachineApplicable`. It never *writes* a silence directive on
//! the author's behalf — deleting a stale one is the mechanical inverse, and
//! the renderers still print each finding's "if intentional, silence with:"
//! hint for a human to paste.
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

use wl_diagnostic::{Applicability, Diagnostic, Suggestion};

/// What a `--fix` pass did — the caller keys follow-up hints on it.
#[derive(Default)]
pub(crate) struct FixSummary {
    /// Files actually rewritten.
    pub(crate) modified: usize,
    /// Some applied suggestion was a deletion (empty replacement) — the
    /// residue may be valid-but-unformatted, worth a `cargo fmt` hint.
    pub(crate) deleted_any: bool,
}

/// Apply machine-applicable structural suggestions to disk. Everything in
/// `Diagnostic::suggestions` is structural by construction — replacements,
/// deletions, and pure insertions (a zero-width span, e.g. the
/// centralized-deps `[workspace.dependencies]` seed) alike. Silence hints
/// never enter that list: the renderers synthesize them from the
/// `SilenceAnchor` at render time (`Diagnostic::silence_suggestion`), which is
/// what keeps "`--fix` never writes a silence directive" structural rather
/// than filtered.
pub(crate) fn run(diagnostics: &[Diagnostic]) -> FixSummary {
    let mut structural_count = 0usize;
    let mut withheld_count = 0usize;
    let mut candidates: Vec<Suggestion> = Vec::new();
    for d in diagnostics {
        for s in &d.suggestions {
            if s.applicability == Applicability::MachineApplicable {
                structural_count += 1;
                candidates.push(s.clone());
            } else {
                withheld_count += 1;
            }
        }
    }

    if candidates.is_empty() {
        if withheld_count > 0 {
            eprintln!(
                "workspace-lint --fix: no machine-applicable fixes — {withheld_count} \
                 suggestion{} withheld as possibly incorrect (each finding says why)",
                if withheld_count == 1 { "" } else { "s" },
            );
        } else {
            eprintln!("workspace-lint --fix: no structural fixes available");
        }
        return FixSummary::default();
    }

    eprintln!(
        "workspace-lint --fix: applying {structural_count} structural fix{}",
        if structural_count == 1 { "" } else { "es" },
    );
    if withheld_count > 0 {
        eprintln!(
            "workspace-lint --fix: {withheld_count} more suggestion{} withheld as possibly \
             incorrect (each finding says why)",
            if withheld_count == 1 { "" } else { "s" },
        );
    }

    let mut by_file: BTreeMap<std::path::PathBuf, Vec<Suggestion>> = BTreeMap::new();
    for s in candidates {
        by_file.entry(s.span.file.clone()).or_default().push(s);
    }

    // Never write into a file git does not track: gitignored/generated files
    // (a build.rs output reached via `include!`) have no `git checkout`
    // backup, and untracked sources are someone's work in progress. Outside a
    // repo (fixture tempdirs) everything counts as writable.
    let skipped = drop_untracked_files(&mut by_file);
    if skipped > 0 {
        eprintln!(
            "workspace-lint --fix: skipped {skipped} fix{} in untracked or gitignored files",
            if skipped == 1 { "" } else { "es" }
        );
    }

    let mut summary = FixSummary::default();
    for (path, suggestions) in by_file {
        match apply_to_file(&path, &suggestions) {
            Ok(true) => {
                summary.modified += 1;
                summary.deleted_any |= suggestions.iter().any(Suggestion::is_deletion);
            }
            Ok(false) => {}
            Err(e) => {
                eprintln!("warning: failed to fix {}: {e}", path.display());
            }
        }
    }

    if summary.modified > 0 {
        eprintln!(
            "workspace-lint --fix: modified {} file{}",
            summary.modified,
            if summary.modified == 1 { "" } else { "s" }
        );
    } else {
        eprintln!("workspace-lint --fix: no files modified");
    }
    summary
}

/// Remove entries for files git does not track (one batched `git ls-files`
/// over the whole set), returning the number of suggestions dropped. Outside
/// a git repository — fixture tempdirs — nothing is dropped, mirroring
/// `ensure_clean_for_fix`'s not-a-repo posture.
fn drop_untracked_files(by_file: &mut BTreeMap<std::path::PathBuf, Vec<Suggestion>>) -> usize {
    if by_file.is_empty() {
        return 0;
    }
    let repo_probe = wl_lint_api::git::command(std::path::Path::new("."))
        .args(["rev-parse", "--is-inside-work-tree"])
        .output();
    match repo_probe {
        Ok(out) if out.status.success() => {}
        _ => return 0, // not a repo (or no git) — apply everything
    }
    let mut ls = wl_lint_api::git::command(std::path::Path::new("."));
    ls.args(["ls-files", "-z", "--"]);
    for path in by_file.keys() {
        ls.arg(path);
    }
    let Ok(out) = ls.output() else { return 0 };
    if !out.status.success() {
        return 0;
    }
    // Lint anchors are mixed relative/absolute and `ls-files` prints
    // cwd-relative paths — normalize both sides to absolute (no symlink
    // resolution; both sides resolve against the same cwd) before comparing.
    let tracked: std::collections::BTreeSet<std::path::PathBuf> =
        String::from_utf8_lossy(&out.stdout)
            .split('\0')
            .filter(|p| !p.is_empty())
            .filter_map(|p| std::path::absolute(p).ok())
            .collect();
    let mut skipped = 0usize;
    by_file.retain(|path, suggestions| {
        // Skip only on positive evidence — an unnormalizable path stays in.
        let untracked = std::path::absolute(path).is_ok_and(|abs| !tracked.contains(&abs));
        if untracked {
            skipped += suggestions.len();
            eprintln!(
                "workspace-lint --fix: skipping `{}` (untracked or gitignored — no git backup)",
                path.display()
            );
        }
        !untracked
    });
    skipped
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

    // Pure deletions may legitimately overlap (adjacent deleted items share
    // their blank-separator run, an EOF deletion reaches back over it) —
    // union them into disjoint ranges. Rewrites must not overlap anything:
    // if a lint ever emits such a pair, the right call is to fix the lint,
    // not silently corrupt the file.
    let mut deletions: Vec<(usize, usize)> = Vec::new();
    let mut rewrites: Vec<&Suggestion> = Vec::new();
    for s in to_apply {
        if s.is_deletion() {
            deletions.push((s.span.byte_start as usize, s.span.byte_end as usize));
        } else {
            rewrites.push(s);
        }
    }
    let deletions = wl_lint_api::surgery::lines::coalesce(&mut deletions);

    let mut ranges: Vec<(usize, usize)> = rewrites
        .iter()
        .map(|s| (s.span.byte_start as usize, s.span.byte_end as usize))
        .chain(deletions.iter().copied())
        .collect();
    ranges.sort_unstable();
    for w in ranges.windows(2) {
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

    // Apply by descending byte offset so earlier offsets stay valid as we
    // mutate from the back. `line_start` is the tiebreaker for deterministic
    // behavior when two rewrites share a byte_start.
    let mut ops: Vec<(usize, usize, Cow<'_, str>)> = rewrites
        .iter()
        .map(|s| {
            (
                s.span.byte_start as usize,
                s.span.byte_end as usize,
                normalize_eol(&s.replacement, eol),
            )
        })
        .chain(deletions.iter().map(|&(lo, hi)| (lo, hi, Cow::from(""))))
        .collect();
    ops.sort_by_key(|op| std::cmp::Reverse(op.0));
    let mut fixed = source.clone();
    for (start, end, replacement) in ops {
        fixed.replace_range(start..end, replacement.as_ref());
    }

    // A deletion run that reached EOF turns the blank separator above it into
    // trailing blank lines — fmt-dirty residue only visible here, after
    // adjacent deletions merged. Trim to exactly one final newline.
    if !deletions.is_empty() && !fixed.is_empty() {
        wl_lint_api::surgery::lines::trim_trailing_blank_lines(&mut fixed, eol);
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
    use std::borrow::Cow;
    use tempfile::TempDir;
    use wl_diagnostic::builder::at_file;
    use wl_diagnostic::{Diagnostic, Level, SilenceAnchor, Span};

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
                original: None,
            }],
            silence_anchor: SilenceAnchor::File {
                file: path.to_path_buf(),
            },
            level_is_explicit: false,
            marker_available: false,
        }
    }

    #[test]
    fn fix_with_no_diagnostics_is_a_noop() {
        let modified = run(&[]).modified;
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

        let d = at_file("workspace-lint::file-size", "file too big", p.clone()).build();

        let modified = run(&[d]).modified;
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
            original: None,
        };
        let modified = apply_to_file(&p, std::slice::from_ref(&s)).unwrap();
        assert!(modified);
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "pub(crate) fn x() {}\n"
        );
    }

    #[test]
    fn fix_applies_whole_line_deletion() {
        // An empty replacement whose span covers a line incl. its newline —
        // the shape stale-expect and unused-deps produce — removes the line.
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("lib.rs");
        let body = "keep_a();\nworkspace_lint::expect!(unused_pub);\nkeep_b();\n";
        std::fs::write(&p, body).unwrap();
        let line2_start = body.find("workspace_lint").unwrap();
        let line2_end = body[line2_start..].find('\n').unwrap() + line2_start + 1;
        let s = Suggestion {
            span: Span {
                file: p.clone(),
                line_start: 2,
                line_end: 2,
                col_start: 1,
                col_end: 1,
                byte_start: line2_start as u32,
                byte_end: line2_end as u32,
            },
            message: "remove this stale expect directive".into(),
            replacement: String::new(),
            applicability: Applicability::MachineApplicable,
            original: Some("workspace_lint::expect!(unused_pub);".into()),
        };
        let modified = apply_to_file(&p, std::slice::from_ref(&s)).unwrap();
        assert!(modified);
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "keep_a();\nkeep_b();\n"
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
        let modified = run(std::slice::from_ref(&d)).modified;
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
            original: None,
        };
        let mut b = a.clone();
        b.span.byte_start = 3;
        b.span.byte_end = 8;
        let err = apply_to_file(&p, &[a, b]).unwrap_err();
        assert!(err.to_string().contains("overlapping"));
    }

    #[test]
    fn fix_merges_overlapping_deletions() {
        // Adjacent deleted items legitimately share their blank-separator run
        // (an EOF deletion reaches back over it) — pure deletions union
        // instead of refusing. A rewrite overlapping a deletion still refuses.
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("lib.rs");
        std::fs::write(&p, "fn a() {}\n\nfn b() {}\n").unwrap();
        let del = |lo: u32, hi: u32| Suggestion {
            span: Span {
                file: p.clone(),
                line_start: 1,
                line_end: 1,
                col_start: 1,
                col_end: 1,
                byte_start: lo,
                byte_end: hi,
            },
            message: "delete".into(),
            replacement: String::new(),
            applicability: Applicability::MachineApplicable,
            original: None,
        };
        // fn a + separator [0,11) overlaps separator + fn b [10,21).
        assert!(apply_to_file(&p, &[del(0, 11), del(10, 21)]).unwrap());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "");
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
