//! The `--stats` measurement pass: everything the threshold-tuning readout
//! needs about one run, with none of the lint's gating applied. This is the
//! evidence surface `max-parameters` and the drift guards are calibrated
//! against — it reports what the lint *would* judge, never judges itself.

use std::path::PathBuf;

use wl_engine::fast::FastModel;
use wl_engine::fast::clones::divergence::{Divergence, DivergenceAnalyzer};
use wl_engine::fast::clones::{CandidateKind, CloneGroup, find_clones};

use super::{DuplicateCodeConfig, enumerate, options};

/// One run's raw measurements: every group `find_clones` returns, plus the
/// cross-group adjacency counts.
pub struct MeasureReport {
    /// Per-group measurements, in group report order.
    pub groups: Vec<GroupMeasure>,
    /// Unordered group pairs whose corresponding instances all sit within G
    /// source lines of each other at every site, indexed by G−1 for
    /// G ∈ 1..=5 — the prey count for gap-tolerant stitching.
    pub stitchable: [usize; 5],
}

/// One clone group's measurements.
pub struct GroupMeasure {
    /// Anchor file (the group's first instance).
    pub file: PathBuf,
    /// Anchor line (1-based).
    pub line: u32,
    /// Number of instances in the group.
    pub instances: usize,
    /// The candidate shape the group was found as.
    pub kind: CandidateKind,
    /// Shared normalized-token weight.
    pub tokens: usize,
    /// Source lines the anchor instance spans.
    pub lines: u32,
    /// Literal divergence; `None` under `--exact-literals` (instances are
    /// literal-identical by construction) or on a capture misalignment.
    pub divergence: Option<Divergence>,
}

/// Scan and measure without judging: every group, its divergence, and the
/// adjacency counts.
pub fn measure(fast: &FastModel, config: &DuplicateCodeConfig) -> MeasureReport {
    let files = enumerate(fast, config);
    let groups = find_clones(&files, &options(config));
    let mut analyzer = DivergenceAnalyzer::new(&files);
    let measures = groups
        .iter()
        .map(|g| {
            let anchor = &g.instances[0];
            GroupMeasure {
                file: anchor.file.clone(),
                line: anchor.line_start,
                instances: g.instances.len(),
                kind: anchor.kind,
                tokens: g.tokens,
                lines: anchor.line_end - anchor.line_start + 1,
                divergence: config
                    .ignore_literals
                    .then(|| analyzer.analyze(g))
                    .flatten(),
            }
        })
        .collect();
    MeasureReport {
        groups: measures,
        stitchable: stitchable(&groups),
    }
}

/// For each gap G ∈ 1..=5, how many unordered group pairs could stitch: same
/// instance count, every corresponding instance pair in the same file with
/// the later one starting at most G lines after the earlier one ends.
/// Overlapping pairs (gap 0) are subsumption leftovers, not stitching prey.
fn stitchable(groups: &[CloneGroup]) -> [usize; 5] {
    let mut out = [0; 5];
    for (i, a) in groups.iter().enumerate() {
        for b in &groups[i + 1..] {
            if a.instances.len() != b.instances.len() {
                continue;
            }
            let gap = a
                .instances
                .iter()
                .zip(&b.instances)
                .try_fold(0u32, |max, (ra, rb)| {
                    if ra.file != rb.file {
                        return None;
                    }
                    // Whichever region comes later, its start minus the
                    // other's end; overlap saturates both directions to 0.
                    let gap = rb
                        .line_start
                        .saturating_sub(ra.line_end)
                        .max(ra.line_start.saturating_sub(rb.line_end));
                    Some(max.max(gap))
                });
            if let Some(gap @ 1..=5) = gap {
                for slot in &mut out[gap as usize - 1..] {
                    *slot += 1;
                }
            }
        }
    }
    out
}
