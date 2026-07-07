//! The `--fix-auto-delete` deletion surface: whole-item removal byte math shared by the
//! pub findings (`ir`), the private-collateral cascade (`cascade`), and the
//! import surgery (`surgery`).
//!
//! The load-bearing subtlety is the **lexical attribute extension**: the
//! extractor's `full_span` covers doc comments but not every attribute —
//! `#[cfg(…)]` is stripped from HIR in the unit where the item survives, and
//! an attribute-macro is consumed before HIR exists — so the deletion start
//! extends backward over the contiguous outer-attribute stack at fix time,
//! on the on-disk source. Deletions are `MachineApplicable` only for
//! git-tracked-clean files (git is the backup).

use std::path::Path;

use wl_engine::wl_ir;

use wl_diagnostic::{Applicability, PubVerdict};

/// Pick a deletion suggestion when the run asked for one (`auto_delete`, i.e.
/// `--fix-auto-delete`) and
/// the item is genuinely unused. Returns `None` to mean "fall back to the
/// tightening suggestion". The `Option<String>` second element carries the
/// "git-dirty file" caveat note when present.
pub(super) fn pick_deletion_fix(
    auto_delete: bool,
    file: &Path,
    span: &wl_ir::Span,
    verdict: PubVerdict,
) -> Option<(wl_diagnostic::Suggestion, Option<String>)> {
    if !auto_delete || verdict != PubVerdict::Unused {
        return None;
    }
    match delete_suggestion(file, span) {
        DeleteOutcome::Apply(s) => Some((s, None)),
        DeleteOutcome::Skip(s, reason) => Some((s, Some(reason))),
        DeleteOutcome::Unavailable => None,
    }
}

pub(super) enum DeleteOutcome {
    /// Git-tracked-clean: emit a MachineApplicable deletion suggestion.
    Apply(wl_diagnostic::Suggestion),
    /// Tracked-but-dirty or untracked: emit MaybeIncorrect so `--fix` passes
    /// over it, plus a reason note for the user.
    Skip(wl_diagnostic::Suggestion, String),
    /// File can't be read, degenerate range, etc. Fall back to the
    /// visibility-narrowing path.
    Unavailable,
}

pub(super) fn delete_suggestion(file: &Path, span: &wl_ir::Span) -> DeleteOutcome {
    let Ok(source) = fs_err::read_to_string(file) else {
        return DeleteOutcome::Unavailable;
    };
    let mut start = span.lo as usize;
    let mut end = (span.hi as usize).min(source.len());
    if start >= end {
        return DeleteOutcome::Unavailable;
    }
    // The extractor's `full_span` covers doc comments but NOT every attribute:
    // `#[cfg(...)]` is stripped from HIR in the unit where the item survives,
    // and an attribute-macro (`#[tracing::instrument]`) is consumed before HIR
    // exists — deleting the span as-is orphans those attrs onto whatever
    // follows (a syntax error before `}`, a silent re-target otherwise). Outer
    // attributes bind to the item below them, so extending the deletion
    // backward over contiguous `#[…]` blocks (and doc lines) is always sound.
    start = extend_over_preceding_attrs(&source, start);
    // Eat the item's leading indentation (horizontal whitespace back to the
    // line start) so a deleted nested item leaves no orphaned indent. Safe: it
    // only crosses spaces/tabs, never a newline or another item's text.
    let bytes = source.as_bytes();
    while start > 0 && matches!(bytes[start - 1], b' ' | b'\t') {
        start -= 1;
    }
    // The item text itself (sans the trailing newline the deletion also
    // eats), for the rendered `-` diff line.
    let original = source[start..end].to_string();
    if end < source.len() && source.as_bytes()[end] == b'\n' {
        end += 1;
    }
    // Also consume whole blank lines below the item: the item's surrounding
    // blank separators would otherwise stack into fmt-dirty residue
    // (`cargo fmt --check` fails on the fixed tree). The blank ABOVE survives
    // as the neighbors' separator; when a deletion run reaches EOF, the
    // applier trims the then-trailing blank lines (only it can see the merged
    // picture across adjacent deletions).
    end = eat_blank_lines(&source, end);
    let applicability = if is_file_clean_in_git(file) {
        Applicability::MachineApplicable
    } else {
        Applicability::MaybeIncorrect
    };
    let suggestion = wl_diagnostic::Suggestion {
        span: wl_diagnostic::Span {
            file: file.to_path_buf(),
            line_start: span.line,
            line_end: span.line,
            col_start: 1,
            col_end: 1,
            byte_start: start as u32,
            byte_end: end as u32,
        },
        message: "delete the unused item".into(),
        replacement: String::new(),
        applicability,
        original: Some(original),
        // Filled in by `attach_pub_evidence` once the diagnostic is built.
    };
    if applicability == Applicability::MachineApplicable {
        DeleteOutcome::Apply(suggestion)
    } else {
        DeleteOutcome::Skip(
            suggestion,
            format!(
                "file `{}` is untracked or has uncommitted changes; `--fix-auto-delete` will not delete it (commit first or use `git stash`)",
                file.display()
            ),
        )
    }
}

/// Extend a deletion's start backward over the contiguous stack of outer
/// attributes (`#[…]`, multiline OK, string literals skipped) and doc-comment
/// lines directly above it. Inner attributes (`#![…]`) bind to the enclosing
/// module, never the item — the scanner refuses them. On any ambiguity the
/// scanner stops: the fallback is today's behavior (attr left in place).
pub(super) fn extend_over_preceding_attrs(source: &str, mut start: usize) -> usize {
    let bytes = source.as_bytes();
    loop {
        // Probe backward over whitespace to the last non-ws char.
        let mut i = start;
        while i > 0 && bytes[i - 1].is_ascii_whitespace() {
            i -= 1;
        }
        if i == 0 {
            return start;
        }
        if bytes[i - 1] == b']' {
            match match_attr_backward(source, i) {
                Some(hash) => {
                    start = hash;
                    continue;
                }
                None => return start,
            }
        }
        // A doc-comment line directly above (`///` — sugar for a doc attr;
        // usually already inside `full_span`, but a macro-consumed item can
        // lose it too). Consume the whole line and keep walking.
        let line_start = source[..i].rfind('\n').map_or(0, |p| p + 1);
        if source[line_start..i].trim_start().starts_with("///") {
            start = line_start;
            continue;
        }
        return start;
    }
}

/// Backward-match one attribute block: `end` is the index just past a `]`;
/// returns the index of the opening `#` when the bracket run balances to an
/// OUTER `#[`. String literals are skipped (a `]` or `[` inside `"…"` is
/// text), with backslash-parity escape handling. `None` on inner attrs
/// (`#![`), unbalanced brackets, or any construct the scanner doesn't model
/// (e.g. raw strings) — never a wrong match, just no extension.
fn match_attr_backward(source: &str, end: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut depth = 0usize;
    let mut i = end;
    while i > 0 {
        i -= 1;
        match bytes[i] {
            b'"' => {
                // Skip backward to the opening quote (an unescaped `"`).
                loop {
                    i = source[..i].rfind('"')?;
                    let backslashes = source[..i]
                        .bytes()
                        .rev()
                        .take_while(|&b| b == b'\\')
                        .count();
                    if backslashes % 2 == 0 {
                        break;
                    }
                }
                // A raw string's hash fence (`r#"…"#`) would leave a stray
                // `#` here and the bracket math can't be trusted — bail.
                if i > 0 && bytes[i - 1] == b'#' {
                    return None;
                }
            }
            b']' => depth += 1,
            b'[' => {
                depth -= 1;
                if depth == 0 {
                    return match i.checked_sub(1) {
                        // `#![…]` is an inner attribute — not the item's.
                        Some(h) if bytes[h] == b'!' => None,
                        Some(h) if bytes[h] == b'#' => Some(h),
                        _ => None,
                    };
                }
            }
            _ => {}
        }
    }
    None
}

/// Consume whole whitespace-only lines starting at `end` (which sits at a
/// line start after the deletion ate its trailing newline).
pub(super) fn eat_blank_lines(source: &str, end: usize) -> usize {
    eat_blank_lines_bytes(source.as_bytes(), end)
}

pub(super) fn eat_blank_lines_bytes(bytes: &[u8], mut end: usize) -> usize {
    loop {
        let mut i = end;
        while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\r') {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'\n' {
            end = i + 1;
        } else {
            return end;
        }
    }
}

/// `true` iff `path` is tracked by git AND has no uncommitted changes.
/// Returns `false` if we can't determine the state — the safer default is to
/// downgrade the suggestion's applicability so `--fix` skips it.
fn is_file_clean_in_git(path: &Path) -> bool {
    if git_success(path, &["ls-files", "--error-unmatch", "--"]).is_none() {
        return false;
    }
    match git_success(path, &["status", "--porcelain", "--"]) {
        Some(out) => out.stdout.is_empty(),
        None => false,
    }
}

/// Run one git subcommand with `path` appended; `Some(output)` only on a
/// zero exit.
fn git_success(path: &Path, args: &[&str]) -> Option<std::process::Output> {
    let out = crate::git::command(Path::new("."))
        .args(args)
        .arg(path)
        .output()
        .ok()?;
    out.status.success().then_some(out)
}
