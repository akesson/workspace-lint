//! Import surgery: the `--fix` cleanup that removes the `use` declarations a
//! cascade deletion leaves dangling. Deleting a `pub fn f` that some module
//! imported with `use crate::m::f;` orphans that import (E0432) unless it goes
//! too — so for every [`DanglingImport`] the semantic model reports, this
//! module computes a byte-exact deletion that always yields *valid* Rust:
//!
//! - **Standalone** `use a::b;` (the leaf item's span is the whole statement,
//!   `decl != elem`) → delete the whole statement (its preceding attributes,
//!   trailing newline, and following blank lines included).
//! - **Brace-list leaf** (`decl == elem`) → excise just the leaf plus one
//!   adjacent separator, leaving live siblings — including ones importing
//!   out-of-workspace items the assembler never sees. `use a::{b, c}` with `b`
//!   removed becomes `use a::{c}`.
//!
//! Ranges are **coalesced per file** before they become suggestions: two dead
//! leaves adjacent in one brace produce overlapping separator ranges, and the
//! `--fix` applier aborts a file on overlapping suggestions. Coalescing only
//! ever merges within-brace neighbours (statements are `;`/newline separated,
//! never byte-adjacent), so it is safe.
//!
//! A brace group all of whose leaves died would be left as `use a::{};` —
//! valid but an `unused_imports` warning under `-D warnings`. Coalescing is
//! therefore interleaved with **group collapse** to a fixpoint: an emptied
//! group is excised as a leaf of *its* parent group, and an emptied top-level
//! tree deletes the whole statement, exactly like the standalone form. Any
//! construct the byte scanner doesn't model bails to the `{}` residue — never
//! a wrong deletion.

pub mod deletion;

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
pub fn import_surgery(dangling: Vec<DanglingImport>, root: &Path) -> Vec<Diagnostic> {
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
        // One raw deletion range per dangling leaf.
        let mut ranges: Vec<Range> = imports
            .iter()
            .filter_map(|d| deletion_range(&src, d))
            .collect();
        let mut merged = coalesce(&mut ranges);
        collapse_emptied_groups(&src, &mut merged);
        for r in merged {
            let original = src.get(r.lo as usize..r.hi as usize).map(str::to_string);
            out.push(deletion_diagnostic(&abs, r.lo, r.hi, r.line, original));
        }
    }
    out
}

/// A byte range `[lo, hi)` to delete, tagged with the 1-based line it starts on
/// (for the diagnostic anchor) and whether it lives inside a brace group (the
/// only ranges group collapse may still widen).
#[derive(Clone, Copy)]
struct Range {
    lo: u32,
    hi: u32,
    line: u32,
    braced: bool,
}

/// The raw deletion range for one dangling leaf, before coalescing. `None` on a
/// degenerate span (unreadable / out of bounds).
fn deletion_range(source: &str, d: &DanglingImport) -> Option<Range> {
    let src = source.as_bytes();
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
        // Whole statement: preceding attribute lines (the decl span starts at
        // the `use` token — a `#[cfg(…)]` above it must go too, same policy
        // as item deletion), the newline, and any following blank lines (a
        // `use` block at the top of a file must not leave a leading blank
        // line behind).
        let lo = deletion::extend_over_preceding_attrs(source, dlo);
        let hi = eat_trailing_newline(src, dhi);
        (lo, deletion::eat_blank_lines_bytes(src, hi))
    };
    Some(Range {
        lo: lo as u32,
        hi: hi as u32,
        line: d.decl.line,
        braced,
    })
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
/// between two dead leaves in one brace (whose separator ranges touch);
/// merging them deletes `b, c` as one span. Statements are never adjacent.
fn coalesce(ranges: &mut [Range]) -> Vec<Range> {
    ranges.sort_by_key(|r| r.lo);
    let mut out: Vec<Range> = Vec::new();
    for &r in ranges.iter() {
        match out.last_mut() {
            Some(last) if r.lo <= last.hi => {
                last.hi = last.hi.max(r.hi);
                last.braced |= r.braced;
            }
            _ => out.push(r),
        }
    }
    out
}

/// Widen brace-leaf ranges whose deletion empties their group, re-coalescing
/// after each pass: `use a::{b}` with `b` dead deletes the whole statement;
/// `use a::{b::{c}, d}` with `c` dead excises the `b::{c}` entry; sibling
/// groups that empty together merge and collapse into their common parent on
/// the next pass. Each widening strictly grows a range, so the loop
/// terminates; a range the scanner can't safely widen is left as-is (the
/// `{}` residue `cargo fmt` at least normalizes).
fn collapse_emptied_groups(source: &str, ranges: &mut Vec<Range>) {
    loop {
        let snapshot = ranges.clone();
        let mut changed = false;
        for r in ranges.iter_mut() {
            if let Some(w) = widen_emptied_group(source, &snapshot, *r) {
                *r = w;
                changed = true;
            }
        }
        if !changed {
            return;
        }
        *ranges = coalesce(ranges);
    }
}

/// One widening step for a brace-leaf range: if deleting `[lo, hi)` leaves its
/// innermost enclosing group with nothing but whitespace and commas, return
/// the range covering the whole `path::{…}` entry (separator included) — or,
/// when the group is the statement's top-level tree, the whole `use …;`
/// statement. `None` when the group keeps live leaves or the surrounding
/// bytes don't match the shapes the scanner models (safe bail).
fn widen_emptied_group(source: &str, all: &[Range], r: Range) -> Option<Range> {
    if !r.braced {
        return None;
    }
    let src = source.as_bytes();
    let (lo, hi) = (r.lo as usize, r.hi as usize);
    let open = enclosing_open_brace(src, lo)?;
    let close = matching_close_brace(src, hi)?;
    // A byte is dead when it is separator noise or inside SOME deletion range
    // — non-adjacent dead leaves of a multiline group never coalesce, so a
    // single range's own bounds can't see the group is emptied.
    let dead = |i: usize| {
        src[i] == b','
            || src[i].is_ascii_whitespace()
            || all
                .iter()
                .any(|o| (o.lo as usize) <= i && i < o.hi as usize)
    };
    if !(open + 1..lo).all(&dead) || !(hi..close).all(&dead) {
        return None; // live siblings (or a comment) — the group survives
    }
    // The entry's path prefix (`b::` in `b::{c}`; empty in `use {a};`).
    let mut s = open;
    while s > 0 && is_path_byte(src[s - 1]) {
        s -= 1;
    }
    // What precedes the entry decides the collapse shape.
    let mut j = s;
    while j > 0 && src[j - 1].is_ascii_whitespace() {
        j -= 1;
    }
    if j > 0 && (src[j - 1] == b',' || src[j - 1] == b'{') {
        // An entry inside a parent group — excise it as a leaf there (one
        // adjacent separator included) and let the next pass judge the parent.
        let (nlo, nhi) = leaf_range(src, s, close + 1);
        return Some(Range {
            lo: nlo as u32,
            hi: nhi as u32,
            braced: true,
            ..r
        });
    }
    // Top level: expect the `use` keyword (with optional visibility before
    // it — `pub use` re-exports never reach surgery, but `pub(crate) use`
    // does), then take the whole statement, mirroring the standalone path.
    let stmt = statement_start(src, j)?;
    let mut end = close + 1;
    while end < src.len() && matches!(src[end], b' ' | b'\t') {
        end += 1;
    }
    if src.get(end) != Some(&b';') {
        return None;
    }
    end = eat_trailing_newline(src, end + 1);
    end = deletion::eat_blank_lines_bytes(src, end);
    let mut start = deletion::extend_over_preceding_attrs(source, stmt);
    while start > 0 && matches!(src[start - 1], b' ' | b'\t') {
        start -= 1;
    }
    Some(Range {
        lo: start as u32,
        hi: end as u32,
        braced: false,
        ..r
    })
}

/// A byte that can appear in a use-tree entry's path prefix: identifier
/// chars, `::` separators, and the `#` of a raw identifier (`r#type::{…}`).
fn is_path_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b':' | b'#')
}

/// The innermost unmatched `{` before `at`, scanning backward. Bails (`None`)
/// on a `;` — the scan left the statement without finding a group, so `at`
/// wasn't inside one after all.
fn enclosing_open_brace(src: &[u8], at: usize) -> Option<usize> {
    let mut depth = 0usize;
    for i in (0..at).rev() {
        match src[i] {
            b'}' => depth += 1,
            b'{' if depth == 0 => return Some(i),
            b'{' => depth -= 1,
            b';' => return None,
            _ => {}
        }
    }
    None
}

/// The `}` matching the group `at` sits in, scanning forward. Same `;` bail.
fn matching_close_brace(src: &[u8], at: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, &b) in src.iter().enumerate().skip(at) {
        match b {
            b'{' => depth += 1,
            b'}' if depth == 0 => return Some(i),
            b'}' => depth -= 1,
            b';' => return None,
            _ => {}
        }
    }
    None
}

/// The statement's first byte, given `end` just past the expected `use`
/// keyword: matches `use`, then an optional `pub` / `pub(…)` visibility
/// before it. `None` when the preceding bytes aren't that shape.
fn statement_start(src: &[u8], end: usize) -> Option<usize> {
    let use_start = keyword_start(src, end, b"use")?;
    let mut j = use_start;
    while j > 0 && src[j - 1].is_ascii_whitespace() {
        j -= 1;
    }
    if j > 0 && src[j - 1] == b')' {
        // `pub(crate)` / `pub(in …)`: no nesting inside a visibility
        // restriction, so the first `(` backward is the opener.
        let p = src[..j - 1].iter().rposition(|&b| b == b'(')?;
        let mut k = p;
        while k > 0 && src[k - 1].is_ascii_whitespace() {
            k -= 1;
        }
        return keyword_start(src, k, b"pub");
    }
    match keyword_start(src, j, b"pub") {
        Some(p) => Some(p),
        None => Some(use_start),
    }
}

/// If the bytes ending at `end` spell `word` on a word boundary, the word's
/// start index.
fn keyword_start(src: &[u8], end: usize, word: &[u8]) -> Option<usize> {
    let start = end.checked_sub(word.len())?;
    if &src[start..end] != word {
        return None;
    }
    let boundary = start == 0 || !src[start - 1].is_ascii_alphanumeric() && src[start - 1] != b'_';
    boundary.then_some(start)
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
