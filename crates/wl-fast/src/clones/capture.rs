//! Lazy per-file literal capture for the divergence pass.
//!
//! Detection abstracts literal values to per-kind placeholders before
//! fingerprinting, so instances of one clone group can differ in their
//! concrete literals. This module recovers those concrete values *after*
//! grouping, from the source spans alone: one flat walk per file collects
//! every literal in source order, and a group instance's literals are the
//! table entries inside its `(line, column)` token range. Nothing here
//! re-runs detection — fingerprints hash interner ids assigned in global
//! first-encounter order across all files, so a partial re-scan could never
//! reproduce them. Cost is proportional to the files that actually contain
//! reported instances.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use proc_macro2::{LineColumn, TokenStream, TokenTree};
use quote::ToTokens;

use super::encode::literal_placeholder;
use super::{Region, ScanFile};

/// One captured literal occurrence.
#[derive(Clone, Debug)]
pub(crate) struct Lit {
    /// The per-kind placeholder (`#str`, `#int`, …) — the same
    /// classification the fingerprint hashed, so instances of one group
    /// always agree on the kind sequence.
    pub kind: &'static str,
    /// Verbatim source text (`"fn"`, `42u8`, …).
    pub text: String,
    /// Start of the literal token (1-based line, 0-based column).
    pub pos: LineColumn,
}

/// Source-ordered literal tables, built once per file on first use.
pub(crate) struct LiteralTables<'a> {
    asts: HashMap<&'a Path, &'a syn::File>,
    tables: HashMap<PathBuf, Vec<Lit>>,
}

impl<'a> LiteralTables<'a> {
    pub(crate) fn new(files: &'a [ScanFile]) -> Self {
        Self {
            asts: files
                .iter()
                .map(|f| (f.rel_path.as_path(), &f.ast))
                .collect(),
            tables: HashMap::new(),
        }
    }

    /// The literals inside `region`, in source order. `None` when the
    /// region's file wasn't among the scanned files (defensive; the caller
    /// derived both from the same scan).
    pub(crate) fn slice(&mut self, region: &Region) -> Option<Vec<Lit>> {
        if !self.tables.contains_key(&region.file) {
            let ast = self.asts.get(region.file.as_path())?;
            let mut table = Vec::new();
            collect(ast.to_token_stream(), &mut table);
            self.tables.insert(region.file.clone(), table);
        }
        let table = &self.tables[&region.file];
        // Half-open [start, end): a literal belongs to the region iff its
        // start position is at or after the region's first token and before
        // the end of its last. Comparing starts only is exact — a token
        // cannot straddle the region boundary, which is itself a token edge.
        let start = LineColumn {
            line: region.line_start as usize,
            column: region.col_start as usize,
        };
        let end = LineColumn {
            line: region.line_end as usize,
            column: region.col_end as usize,
        };
        Some(
            table
                .iter()
                .filter(|l| l.pos >= start && l.pos < end)
                .cloned()
                .collect(),
        )
    }
}

/// Append every literal in `tokens` (recursing into groups) in source order.
/// Doc comments participate: they desugar to `#[doc = "…"]` string literals
/// with genuine spans, exactly as they do in the fingerprint's token stream.
fn collect(tokens: TokenStream, out: &mut Vec<Lit>) {
    for tt in tokens {
        match tt {
            TokenTree::Group(g) => collect(g.stream(), out),
            TokenTree::Literal(l) => out.push(Lit {
                kind: literal_placeholder(&l),
                text: l.to_string(),
                pos: l.span().start(),
            }),
            TokenTree::Ident(_) | TokenTree::Punct(_) => {}
        }
    }
}
