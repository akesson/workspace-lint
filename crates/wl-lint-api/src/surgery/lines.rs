//! Substrate-agnostic line/byte-range primitives shared by every deletion
//! surface: whole-item deletion (`deletion`), import excision (`super`), the
//! TOML dep-line and stale-directive deletions (via the lints/binary), and
//! the `--fix` applier's deletion merge. One CRLF-aware implementation of
//! "widen a deletion over its line residue" instead of the five hand-rolled
//! copies this module replaced.

/// Widen `end` over its line terminator (`\n` or `\r\n`) so the deleted text
/// takes its line with it. No-op when `end` doesn't sit on a terminator.
pub fn eat_trailing_newline(src: &[u8], end: usize) -> usize {
    if end < src.len() && src[end] == b'\n' {
        end + 1
    } else if end + 1 < src.len() && src[end] == b'\r' && src[end + 1] == b'\n' {
        end + 2
    } else {
        end
    }
}

/// Consume whole whitespace-only lines starting at `end` (which sits at a
/// line start, e.g. after [`eat_trailing_newline`]) — a deleted item's blank
/// separators would otherwise stack into fmt-dirty residue.
pub fn eat_blank_lines(src: &[u8], mut end: usize) -> usize {
    loop {
        let mut i = end;
        while i < src.len() && matches!(src[i], b' ' | b'\t' | b'\r') {
            i += 1;
        }
        if i < src.len() && src[i] == b'\n' {
            end = i + 1;
        } else {
            return end;
        }
    }
}

/// Walk `start` back over horizontal whitespace to its line start, so a
/// deleted nested item leaves no orphaned indent. Safe: only crosses
/// spaces/tabs, never a newline or another item's text.
pub(crate) fn eat_leading_indent(src: &[u8], mut start: usize) -> usize {
    while start > 0 && matches!(src[start - 1], b' ' | b'\t') {
        start -= 1;
    }
    start
}

/// Byte range `[start, end)` covering source lines `start_line..=end_line`
/// (1-based) *including* the trailing line terminator (`\n` or `\r\n`), or
/// through EOF for an unterminated final line. `None` if the range is
/// degenerate or runs past the end of the file. CRLF is handled for free: the
/// `\r` lives inside the line, so the range swallows the whole `\r\n`.
pub fn line_span(content: &str, start_line: u32, end_line: u32) -> Option<(usize, usize)> {
    if start_line == 0 || end_line < start_line {
        return None;
    }
    let bytes = content.as_bytes();
    // Byte offset where each line begins; `line_starts[n]` starts line `n + 1`.
    let mut line_starts = vec![0usize];
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\n' {
            line_starts.push(i + 1);
        }
    }
    let s = start_line as usize - 1;
    let e = end_line as usize - 1;
    if s >= line_starts.len() || e >= line_starts.len() {
        return None;
    }
    let byte_start = line_starts[s];
    // End of `end_line` = start of the next line, or EOF for the last line.
    let byte_end = line_starts.get(e + 1).copied().unwrap_or(bytes.len());
    if byte_end <= byte_start {
        return None;
    }
    Some((byte_start, byte_end))
}

/// The applier-side EOF counterpart of [`eat_blank_lines`]: a deletion run
/// that reached EOF turns the blank separator above it into trailing blank
/// lines — residue only visible after adjacent deletions merged. Trim the
/// fixed buffer to exactly one final `eol`.
pub fn trim_trailing_blank_lines(text: &mut String, eol: &str) {
    let body_len = text.trim_end_matches(['\n', '\r', ' ', '\t']).len();
    if body_len > 0 && body_len + eol.len() < text.len() {
        text.truncate(body_len);
        text.push_str(eol);
    }
}

/// A byte range that [`coalesce`] can sort and merge. Implementors carry
/// their own metadata and decide how it merges (see [`ByteRange::merge`]).
pub trait ByteRange {
    fn lo(&self) -> usize;
    fn hi(&self) -> usize;
    /// Absorb an overlapping/adjacent `other` (its `lo` ≤ `self.hi()`):
    /// extend `hi` to the max and merge any metadata.
    fn merge(&mut self, other: &Self);
}

impl ByteRange for (usize, usize) {
    fn lo(&self) -> usize {
        self.0
    }
    fn hi(&self) -> usize {
        self.1
    }
    fn merge(&mut self, other: &Self) {
        self.1 = self.1.max(other.1);
    }
}

/// Merge overlapping or byte-adjacent ranges (`lo <= prev.hi`) into a sorted,
/// disjoint set. The ONE coalescing implementation behind both the import
/// surgery (whose separator ranges of adjacent dead leaves touch) and the
/// `--fix` applier's deletion union (adjacent deleted items share their
/// blank-separator run).
pub fn coalesce<R: ByteRange + Copy>(ranges: &mut [R]) -> Vec<R> {
    ranges.sort_by_key(|r| (r.lo(), r.hi()));
    let mut out: Vec<R> = Vec::new();
    for &r in ranges.iter() {
        match out.last_mut() {
            Some(last) if r.lo() <= last.hi() => last.merge(&r),
            _ => out.push(r),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- eat_trailing_newline ---

    #[test]
    fn eats_lf_and_crlf_terminators() {
        assert_eq!(eat_trailing_newline(b"a\nb", 1), 2);
        assert_eq!(eat_trailing_newline(b"a\r\nb", 1), 3);
        assert_eq!(eat_trailing_newline(b"ab", 1), 1); // not on a terminator
        assert_eq!(eat_trailing_newline(b"a", 1), 1); // EOF
        // A lone `\r` at EOF is not a terminator this models — left alone.
        assert_eq!(eat_trailing_newline(b"a\r", 1), 1);
    }

    // --- eat_blank_lines ---

    #[test]
    fn eats_whitespace_only_lines() {
        assert_eq!(eat_blank_lines(b"\n\t \r\nx", 0), 5);
        assert_eq!(eat_blank_lines(b"  x\n", 0), 0); // line has content
        assert_eq!(eat_blank_lines(b"", 0), 0);
    }

    // --- eat_leading_indent ---

    #[test]
    fn walks_back_over_horizontal_whitespace_only() {
        assert_eq!(eat_leading_indent(b"\n\t  x", 4), 1);
        assert_eq!(eat_leading_indent(b"x", 0), 0);
    }

    // --- line_span (moved verbatim from the stale-expect deletion module) ---

    #[test]
    fn line_span_single_line_lf() {
        // Line 2 of three, LF-terminated.
        let (s, e) = line_span("aaa\nbbb\nccc\n", 2, 2).unwrap();
        assert_eq!(&"aaa\nbbb\nccc\n"[s..e], "bbb\n");
    }

    #[test]
    fn line_span_swallows_crlf() {
        let content = "aaa\r\nbbb\r\nccc\r\n";
        let (s, e) = line_span(content, 1, 1).unwrap();
        assert_eq!(&content[s..e], "aaa\r\n");
    }

    #[test]
    fn line_span_last_line_without_trailing_newline() {
        let content = "aaa\nbbb";
        let (s, e) = line_span(content, 2, 2).unwrap();
        assert_eq!(&content[s..e], "bbb");
    }

    #[test]
    fn line_span_multi_line() {
        let content = "aaa\nbbb\nccc\nddd\n";
        let (s, e) = line_span(content, 2, 3).unwrap();
        assert_eq!(&content[s..e], "bbb\nccc\n");
    }

    #[test]
    fn line_span_out_of_range_is_none() {
        assert_eq!(line_span("aaa\n", 5, 5), None);
        assert_eq!(line_span("aaa\n", 0, 0), None);
        assert_eq!(line_span("aaa\n", 3, 2), None);
    }

    // --- trim_trailing_blank_lines ---

    #[test]
    fn trims_eof_blank_residue_to_one_terminator() {
        let mut s = String::from("fn a() {}\n\n\t\n");
        trim_trailing_blank_lines(&mut s, "\n");
        assert_eq!(s, "fn a() {}\n");

        // Already exactly one terminator: untouched.
        let mut s = String::from("fn a() {}\n");
        trim_trailing_blank_lines(&mut s, "\n");
        assert_eq!(s, "fn a() {}\n");

        // All-whitespace buffer: untouched (no body to anchor the trim).
        let mut s = String::from("\n\n");
        trim_trailing_blank_lines(&mut s, "\n");
        assert_eq!(s, "\n\n");
    }

    // --- coalesce ---

    #[test]
    fn coalesce_merges_overlap_and_adjacency() {
        let mut ranges = vec![(10usize, 21usize), (0, 11), (30, 35), (35, 40)];
        assert_eq!(coalesce(&mut ranges), vec![(0, 21), (30, 40)]);
    }

    #[test]
    fn coalesce_keeps_disjoint_ranges() {
        let mut ranges = vec![(5usize, 6usize), (0, 2)];
        assert_eq!(coalesce(&mut ranges), vec![(0, 2), (5, 6)]);
    }
}
