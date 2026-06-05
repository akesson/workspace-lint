//! SCIP-shaped occurrence projection (the "emitter").
//!
//! Projects the resolved model into a flat, normalized occurrence list aligned
//! with what [SCIP](https://github.com/sourcegraph/scip) (the index
//! `rust-analyzer scip` emits) records per file: `(symbol, role, range)`. This
//! is the in-house twin of a `Workspace → scip::Index` emitter, but it emits a
//! plain `Vec<ScipOccurrence>` rather than a foreign `scip::types::Index` — so
//! the published crate gains **no `scip`/`protobuf` dependency**. The
//! differential harness (`tests/scip_diff.rs`) diffs this against a committed,
//! normalized projection of a pinned rust-analyzer's `.scip`, yielding
//! precision (our false-positive rate) and in-class recall. See
//! `DESIGN-ir-pipeline.md` §10, §5, and §8.
//!
//! ## What it emits, and why it is "in-class" by construction
//!
//! - **Definitions** — every module-level def-kind item ([`is_definition_kind`]).
//! - **References** — every `use`-binding (the imported leaf) and every
//!   resolved, non-macro [`Occurrence`](crate::resolve::Occurrence), each symbol
//!   passed through the `pub use` chain ([`Workspace::resolve_canonical`]) so
//!   re-exports match RA's definition-resolved symbols.
//!
//! The resolver structurally **cannot** produce the SCIP classes that fall
//! outside our remit — method calls (`x.foo()`), field access, inferred-type
//! paths, locals — because it has no type information. That asymmetry is exactly
//! why the differential metric is *in-class recall* (reachable ~100%) rather than
//! global recall (permanently capped). So every occurrence we emit is in-class:
//! [`ScipOccurrence::in_class`] is always `true` here, carried only so the
//! harness can apply one identical filter to both sides.
//!
//! ## Ranges
//!
//! SCIP columns from rust-analyzer are UTF-8 **byte** offsets from the line start
//! (`UTF8CodeUnitOffsetFromLineStart`). [`SourceSpan::column`] is a proc-macro2
//! **char** column, so we derive byte columns from [`SourceSpan::byte_range`]
//! (byte offsets from file start) against a per-file line-start table instead —
//! correct for non-ASCII by construction (the `café` fixture guards this). Path
//! occurrences are single-line, so start and end share a line.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::resolve::{ItemKind, Origin, SourceSpan, Workspace};

/// Whether a SCIP occurrence is a definition or a reference. Mirrors the
/// distinction SCIP encodes in `Occurrence.symbol_roles`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ScipRole {
    Definition,
    Reference,
}

/// One normalized, SCIP-aligned occurrence projected from the resolved model.
///
/// `symbol` is canonical segments (`[code_crate, …]`); `file` is workspace-root-
/// relative; `line`/`start_col`/`end_col` are **0-based** UTF-8 byte offsets
/// (SCIP convention), `end_col` exclusive.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ScipOccurrence {
    pub symbol: Vec<String>,
    pub role: ScipRole,
    pub file: PathBuf,
    pub line: u32,
    pub start_col: u32,
    pub end_col: u32,
    /// Whether this occurrence belongs to the SCIP classes the resolver intends
    /// to produce. Always `true` for emitted occurrences (see module docs);
    /// present for filter symmetry with the rust-analyzer side.
    pub in_class: bool,
    /// `symbol`'s crate (`symbol[0]`) differs from the crate the occurrence
    /// lives in — i.e. a cross-crate reference, the dependency-lint signal.
    pub cross_crate: bool,
}

/// Item kinds projected as SCIP definitions: the named API surface
/// ([`ItemKind::is_definition`]) **minus `Macro`**, which the fixtures don't
/// declare and the rustdoc/SCIP oracle distillers also exclude. This is the
/// single source of truth for the syn-side def classification — `tests/oracle.rs`
/// and the SCIP harness both call it, so the classification can't silently drift
/// (do **not** substitute `ItemKind::is_definition`, which includes `Macro`).
pub fn is_definition_kind(kind: ItemKind) -> bool {
    kind.is_definition() && kind != ItemKind::Macro
}

impl Workspace {
    /// Project the resolved model into a normalized, SCIP-aligned occurrence
    /// list. See the module documentation for the emission rules and the
    /// in-class guarantee.
    pub fn scip_occurrences(&self) -> Vec<ScipOccurrence> {
        let mut out = Vec::new();
        // file → byte offset of each line's first byte (0-based line index).
        let mut line_starts: HashMap<PathBuf, Option<Vec<usize>>> = HashMap::new();

        for krate in self.members() {
            let owner = krate.code_name();
            for target in &krate.targets {
                for module in target.root.walk() {
                    // Definitions — own-crate by construction (never cross-crate).
                    for item in &module.items {
                        if !is_definition_kind(item.kind) {
                            continue;
                        }
                        if let Some(span) = &item.source {
                            self.push(
                                &mut out,
                                &mut line_starts,
                                item.canonical.segments(),
                                ScipRole::Definition,
                                span,
                                &owner,
                            );
                        }
                    }
                    // References — `use` bindings (the imported leaf ident).
                    for binding in &module.use_bindings {
                        if let Some(span) = &binding.source {
                            let canon = self.resolve_canonical(&binding.canonical);
                            self.push(
                                &mut out,
                                &mut line_starts,
                                canon.segments(),
                                ScipRole::Reference,
                                span,
                                &owner,
                            );
                        }
                    }
                    // References — every resolved, non-macro code occurrence.
                    // `Component` / `MacroCall` (bare names a Phase B pass binds)
                    // are out of the in-class set, like `Macro`.
                    for occ in &module.occurrences {
                        if matches!(
                            occ.origin,
                            Origin::Macro | Origin::Component | Origin::MacroCall
                        ) {
                            continue;
                        }
                        let (Some(path), Some(span)) = (&occ.path, &occ.span) else {
                            continue;
                        };
                        let canon = self.resolve_canonical(path);
                        self.push(
                            &mut out,
                            &mut line_starts,
                            canon.segments(),
                            ScipRole::Reference,
                            span,
                            &owner,
                        );
                    }
                }
            }
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn push(
        &self,
        out: &mut Vec<ScipOccurrence>,
        line_starts: &mut HashMap<PathBuf, Option<Vec<usize>>>,
        segments: &[String],
        role: ScipRole,
        span: &SourceSpan,
        owner: &str,
    ) {
        let Some(range) = &span.byte_range else {
            return; // synthetic span — no byte offsets to place a range.
        };
        let starts = line_starts
            .entry(span.file.clone())
            .or_insert_with(|| file_line_starts(&span.file));
        let Some(starts) = starts else { return };
        // `span.line` is 1-based; SCIP lines are 0-based.
        let Some(&line_start) = starts.get(span.line.saturating_sub(1) as usize) else {
            return;
        };
        let (start, mut end) = (range.start as usize, range.end as usize);
        if start < line_start {
            return; // span/line disagree (multi-line or synthetic) — skip.
        }
        // References carry single-line, token-precise spans. `Item::source`,
        // however, covers the whole item (multi-line); clamp the end to the
        // start line so def ranges stay a valid in-line position rather than a
        // bogus byte count. (The harness compares defs symbol-only.)
        if let Some(&next_line_start) = starts.get(span.line as usize) {
            end = end.min(next_line_start);
        }
        let symbol: Vec<String> = segments.to_vec();
        let cross_crate = symbol.first().map(String::as_str) != Some(owner);
        out.push(ScipOccurrence {
            symbol,
            role,
            file: self.crate_relative_path(&span.file),
            line: span.line.saturating_sub(1),
            start_col: (start - line_start) as u32,
            end_col: (end - line_start) as u32,
            in_class: true,
            cross_crate,
        });
    }
}

/// Byte offset of every line's first byte (0-based line index → byte offset).
/// `None` if the file can't be read.
fn file_line_starts(path: &std::path::Path) -> Option<Vec<usize>> {
    let content = std::fs::read(path).ok()?;
    let mut starts = vec![0usize];
    for (i, b) in content.iter().enumerate() {
        if *b == b'\n' {
            starts.push(i + 1);
        }
    }
    Some(starts)
}
