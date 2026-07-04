//! Import surgery: the `--fix` cleanup that removes the `use` declarations a
//! cascade deletion leaves dangling. Deleting a `pub fn f` that some module
//! imported with `use crate::m::f;` orphans that import (E0432) unless it goes
//! too — so for every [`DanglingImport`] the semantic model reports, this
//! module computes a byte-exact deletion that always yields *valid* Rust:
//!
//! - **Standalone** `use a::b;` (the leaf item's span is the whole statement,
//!   `decl != elem`) → delete the whole statement (plus its trailing newline).
//! - **Brace-list leaf** (`decl == elem`) → excise just the leaf plus one
//!   adjacent separator, leaving live siblings — including ones importing
//!   out-of-workspace items the assembler never sees. `use a::{b, c}` with `b`
//!   removed becomes `use a::{c}`; with both removed, `use a::{}` (valid, and
//!   normalized away by `cargo fmt`).
//!
//! Ranges are **coalesced per file** before they become suggestions: two dead
//! leaves adjacent in one brace produce overlapping separator ranges, and the
//! `--fix` applier aborts a file on overlapping suggestions. Coalescing only
//! ever merges within-brace neighbours (statements are `;`/newline separated,
//! never byte-adjacent), so it is safe.

use std::collections::BTreeMap;
use std::path::Path;

use wl_engine::semantic::DanglingImport;

use crate::LintId;
use wl_diagnostic::{Applicability, Diagnostic, Span, Suggestion, builder::at_line};

/// Build the coalesced import-deletion diagnostics for a cascade's dangling
/// imports. `root` joins the workspace-relative span files. Imports that can't
/// be safely excised (macro-generated, or `pub use` re-exports) are skipped —
/// the cascade already refused to delete their targets
/// (`SemanticModel::import_excision_blocked`), so they never dangle here.
pub(crate) fn import_surgery(dangling: Vec<DanglingImport>, root: &Path) -> Vec<Diagnostic> {
    // Group excisable leaves by file.
    let mut by_file: BTreeMap<String, Vec<DanglingImport>> = BTreeMap::new();
    for d in dangling {
        if d.reexport || d.decl.from_expansion || d.elem.from_expansion {
            continue; // un-editable; the target was withheld from removal
        }
        by_file.entry(d.decl.file.clone()).or_default().push(d);
    }

    let mut out = Vec::new();
    for (file, imports) in by_file {
        let abs = root.join(&file);
        let Ok(src) = fs_err::read_to_string(&abs) else {
            continue;
        };
        let bytes = src.as_bytes();
        // One raw deletion range per dangling leaf.
        let mut ranges: Vec<Range> = imports
            .iter()
            .filter_map(|d| deletion_range(bytes, d))
            .collect();
        for (lo, hi, line) in coalesce(&mut ranges) {
            let original = src.get(lo as usize..hi as usize).map(str::to_string);
            out.push(deletion_diagnostic(&abs, lo, hi, line, original));
        }
    }
    out
}

/// A byte range `[lo, hi)` to delete, tagged with the 1-based line it starts on
/// (for the diagnostic anchor).
type Range = (u32, u32, u32);

/// The raw deletion range for one dangling leaf, before coalescing. `None` on a
/// degenerate span (unreadable / out of bounds).
fn deletion_range(src: &[u8], d: &DanglingImport) -> Option<Range> {
    let elo = d.elem.lo as usize;
    let ehi = (d.elem.hi as usize).min(src.len());
    let dlo = d.decl.lo as usize;
    let dhi = (d.decl.hi as usize).min(src.len());
    if elo >= ehi || dlo >= dhi {
        return None;
    }
    // Brace discriminator: the extractor collapses a brace-list leaf's item
    // span to the leaf, so `decl == elem` ⇒ excise in place; otherwise `decl`
    // is the whole `use …;` statement.
    let braced = d.decl.lo == d.elem.lo && d.decl.hi == d.elem.hi;
    let (lo, hi) = if braced {
        leaf_range(src, elo, ehi)
    } else {
        (dlo, eat_trailing_newline(src, dhi))
    };
    Some((lo as u32, hi as u32, d.decl.line))
}

/// A brace-list leaf's deletion range: the leaf plus **one** adjacent
/// separator. Trailing `,` (and following horizontal whitespace) is preferred;
/// a last leaf with no trailing comma consumes its leading `,` instead; a sole
/// leaf (`use a::{b}`) consumes neither, leaving `use a::{}`.
fn leaf_range(src: &[u8], elo: usize, ehi: usize) -> (usize, usize) {
    let horiz = |b: u8| b == b' ' || b == b'\t';
    // Trailing: skip horizontal ws, then a comma + trailing horizontal ws.
    let mut t = ehi;
    while t < src.len() && horiz(src[t]) {
        t += 1;
    }
    if t < src.len() && src[t] == b',' {
        t += 1;
        while t < src.len() && horiz(src[t]) {
            t += 1;
        }
        return (elo, t);
    }
    // Leading (last leaf): a comma + preceding horizontal ws before the leaf.
    let mut l = elo;
    while l > 0 && horiz(src[l - 1]) {
        l -= 1;
    }
    if l > 0 && src[l - 1] == b',' {
        l -= 1;
        while l > 0 && horiz(src[l - 1]) {
            l -= 1;
        }
        return (l, ehi);
    }
    (elo, ehi) // sole leaf
}

/// Extend a whole-statement deletion over its line-terminating newline
/// (CRLF-safe) so no blank line is left — mirroring the item-deletion policy.
fn eat_trailing_newline(src: &[u8], hi: usize) -> usize {
    if hi < src.len() && src[hi] == b'\n' {
        hi + 1
    } else if hi + 1 < src.len() && src[hi] == b'\r' && src[hi + 1] == b'\n' {
        hi + 2
    } else {
        hi
    }
}

/// Merge overlapping or byte-adjacent ranges. Adjacency only ever occurs
/// between two dead leaves in one brace (whose separator ranges touch); merging
/// them deletes `b, c` as one span, leaving `{}`. Statements are never adjacent.
fn coalesce(ranges: &mut [Range]) -> Vec<Range> {
    ranges.sort_by_key(|(lo, _, _)| *lo);
    let mut out: Vec<Range> = Vec::new();
    for &(lo, hi, line) in ranges.iter() {
        match out.last_mut() {
            Some(last) if lo <= last.1 => {
                last.1 = last.1.max(hi);
            }
            _ => out.push((lo, hi, line)),
        }
    }
    out
}

fn deletion_diagnostic(
    file: &Path,
    lo: u32,
    hi: u32,
    line: u32,
    original: Option<String>,
) -> Diagnostic {
    at_line(
        LintId::UnusedPub.id(),
        "unused import of a removed item".to_string(),
        file.to_path_buf(),
        line,
    )
    .help("removing the dangling `use` left by the deleted item")
    .suggestion(Suggestion {
        span: Span {
            file: file.to_path_buf(),
            line_start: line,
            line_end: line,
            col_start: 1,
            col_end: 1,
            byte_start: lo,
            byte_end: hi,
        },
        message: "remove the unused import".into(),
        replacement: String::new(),
        applicability: Applicability::MachineApplicable,
        original,
    })
    .build()
}

#[cfg(test)]
mod tests;
