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

use super::lines;
use wl_diagnostic::{Applicability, PubVerdict};

/// Pick a deletion suggestion when the run asked for one (`auto_delete`, i.e.
/// `--fix-auto-delete`) and
/// the item is genuinely unused. Returns `None` to mean "fall back to the
/// tightening suggestion". The `Option<String>` second element carries the
/// "git-dirty file" caveat note when present.
pub fn pick_deletion_fix(
    auto_delete: bool,
    file: &Path,
    span: &wl_ir::Span,
    verdict: PubVerdict,
) -> Option<(wl_diagnostic::Suggestion, Option<String>)> {
    // TestOnly gets a deletion surface in auto-delete mode too, but only the
    // cascade ever calls with `auto_delete = true`, and it gates the delete
    // behind the exclusive-test-scaffolding proof (deleting the referencing
    // tests in the same pass, or downgrading this suggestion with a blocker
    // note). The plain-check path always passes `auto_delete = false`.
    if !auto_delete || !matches!(verdict, PubVerdict::Unused | PubVerdict::TestOnly) {
        return None;
    }
    match delete_suggestion(file, span) {
        DeleteOutcome::Apply(s) => Some((s, None)),
        DeleteOutcome::Skip(s, reason) => Some((s, Some(reason))),
        DeleteOutcome::Unavailable => None,
    }
}

pub enum DeleteOutcome {
    /// Git-tracked-clean: emit a MachineApplicable deletion suggestion.
    Apply(wl_diagnostic::Suggestion),
    /// No git backup (dirty, untracked, or no repo at all): emit
    /// MaybeIncorrect so `--fix` passes over it, plus a reason note.
    Skip(wl_diagnostic::Suggestion, String),
    /// File can't be read, degenerate range, etc. Fall back to the
    /// visibility-narrowing path.
    Unavailable,
}

/// Which fix flag applies a deletion — selects only the flag name rendered in
/// the withhold note. Line-grain deletions (a TOML dep line, a stale
/// directive line) apply under plain `--fix`; whole-item deletion is the
/// `--fix-auto-delete` escalation.
#[derive(Clone, Copy)]
pub enum FixFlag {
    Fix,
    AutoDelete,
}

impl FixFlag {
    fn as_str(self) -> &'static str {
        match self {
            FixFlag::Fix => "--fix",
            FixFlag::AutoDelete => "--fix-auto-delete",
        }
    }
}

/// Build an empty-replacement deletion [`wl_diagnostic::Suggestion`] with the
/// uniform per-file git gate applied: `MachineApplicable` iff the file is
/// git-tracked-and-clean (git is the deletion's backup), withheld otherwise
/// with the reason returned as the second element — `None` means applicable.
/// The ONE gate every deletion kind goes through: Rust items (via
/// [`delete_suggestion`]), TOML dep lines, and stale directive lines.
pub fn gated_deletion_suggestion(
    file: &Path,
    bytes: (usize, usize),
    lines: (u32, u32),
    message: String,
    original: Option<String>,
    flag: FixFlag,
) -> (wl_diagnostic::Suggestion, Option<String>) {
    let reason = match crate::git::file_state(file) {
        crate::git::FileState::CleanTracked => None,
        crate::git::FileState::NoRepo => Some(format!(
            "file `{}` is not in a git repository — no backup to restore from; `{}` will not delete it",
            file.display(),
            flag.as_str()
        )),
        crate::git::FileState::DirtyOrUntracked => Some(format!(
            "file `{}` is untracked or has uncommitted changes; `{}` will not delete it (commit first or use `git stash`)",
            file.display(),
            flag.as_str()
        )),
    };
    let suggestion = wl_diagnostic::Suggestion {
        span: wl_diagnostic::Span {
            file: file.to_path_buf(),
            line_start: lines.0,
            line_end: lines.1,
            col_start: 1,
            col_end: 1,
            byte_start: bytes.0 as u32,
            byte_end: bytes.1 as u32,
        },
        message,
        replacement: String::new(),
        applicability: if reason.is_none() {
            Applicability::MachineApplicable
        } else {
            Applicability::MaybeIncorrect
        },
        original,
    };
    (suggestion, reason)
}

pub fn delete_suggestion(file: &Path, span: &wl_ir::Span) -> DeleteOutcome {
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
    // Eat the item's leading indentation so a deleted nested item leaves no
    // orphaned indent.
    let bytes = source.as_bytes();
    start = lines::eat_leading_indent(bytes, start);
    // The item text itself (sans the trailing newline the deletion also
    // eats), for the rendered `-` diff line.
    let original = source[start..end].to_string();
    end = lines::eat_trailing_newline(bytes, end);
    // Also consume whole blank lines below the item: the item's surrounding
    // blank separators would otherwise stack into fmt-dirty residue
    // (`cargo fmt --check` fails on the fixed tree). The blank ABOVE survives
    // as the neighbors' separator; when a deletion run reaches EOF, the
    // applier trims the then-trailing blank lines (only it can see the merged
    // picture across adjacent deletions).
    end = lines::eat_blank_lines(bytes, end);
    let (suggestion, reason) = gated_deletion_suggestion(
        file,
        (start, end),
        (span.line, span.line),
        "delete the unused item".into(),
        Some(original),
        FixFlag::AutoDelete,
    );
    match reason {
        None => DeleteOutcome::Apply(suggestion),
        Some(reason) => DeleteOutcome::Skip(suggestion, reason),
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
