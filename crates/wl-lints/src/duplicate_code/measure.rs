//! The `--stats` measurement pass: everything the threshold-tuning readout
//! needs about one run, with none of the lint's gating applied. This is the
//! evidence surface `max-parameters` and the drift guards are calibrated
//! against — it reports what the lint *would* judge, never judges itself.

use std::path::PathBuf;

use wl_engine::fast::FastModel;
use wl_engine::fast::clones::divergence::{Divergence, DivergenceAnalyzer};
use wl_engine::fast::clones::liveness::{Liveness, LivenessAnalyzer};
use wl_engine::fast::clones::{CandidateKind, CloneGroup, find_clones};

use super::classify::Classifier;
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
    /// The extraction signature for a statement-run group; `None` for fn/block
    /// groups and runs with no enclosing fn. Purely syntactic, so it is
    /// available on this build-free path.
    pub liveness: Option<Liveness>,
    /// The group's SYNTACTIC refactoring class (kebab-case). Measure-only and
    /// build-free, so the call-graph verdicts (`merge-identical-fns` /
    /// `delete-dead-copy` / `merge-withheld`) never appear here — only
    /// `default-trait-method` / `method-on-receiver-type` / `ui-component` /
    /// `unclassified`.
    pub class: &'static str,
}

/// Scan and measure without judging: every group, its divergence, and the
/// adjacency counts.
pub fn measure(fast: &FastModel, config: &DuplicateCodeConfig) -> MeasureReport {
    let files = enumerate(fast, config);
    let groups = find_clones(&files, &options(config));
    let mut analyzer = DivergenceAnalyzer::new(&files);
    let mut liveness = LivenessAnalyzer::new(&files);
    // The readout classifies syntactically (no semantic model), so the
    // call-graph verdicts never appear — measure-only, and build-free.
    let mut classifier = Classifier::new(&files, None, &config.component_macros);
    let measures = groups
        .iter()
        .map(|g| {
            let anchor = &g.instances[0];
            let divergence = config
                .ignore_literals
                .then(|| analyzer.analyze(g))
                .flatten();
            let identical = divergence
                .as_ref()
                .is_none_or(|d| d.params == 0 && d.violations.is_empty());
            GroupMeasure {
                file: anchor.file.clone(),
                line: anchor.line_start,
                instances: g.instances.len(),
                kind: anchor.kind,
                tokens: g.tokens,
                lines: anchor.line_end - anchor.line_start + 1,
                class: classifier.classify(g, identical).label(),
                divergence,
                liveness: liveness.analyze(g),
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use wl_engine::fast::clones::Region;

    use super::*;

    fn group(sites: &[(&str, u32, u32)]) -> CloneGroup {
        CloneGroup {
            instances: sites
                .iter()
                .map(|(file, start, end)| Region {
                    file: PathBuf::from(file),
                    krate: "demo".into(),
                    line_start: *start,
                    line_end: *end,
                    col_start: 0,
                    col_end: 1,
                    kind: CandidateKind::Run,
                })
                .collect(),
            tokens: 50,
        }
    }

    /// The adjacency counts are cumulative over G, gap is the worst site,
    /// and pairs disqualify on instance-count or file mismatch; overlap
    /// (gap 0) never counts.
    #[test]
    fn stitchable_counts_cumulative_worst_site_gaps() {
        let groups = [
            // Pair (0,1): gaps 2 (11-9) and 3 (23-20) → stitchable at G>=3.
            group(&[("a.rs", 1, 9), ("b.rs", 12, 20)]),
            group(&[("a.rs", 11, 18), ("b.rs", 23, 30)]),
            // Overlaps group 0 at both sites → gap 0, never counted.
            group(&[("a.rs", 5, 12), ("b.rs", 16, 24)]),
            // Same shape but three instances → count mismatch with all.
            group(&[("a.rs", 40, 44), ("b.rs", 50, 54), ("c.rs", 60, 64)]),
            // Matches group 0's count but different files → disqualified.
            group(&[("x.rs", 1, 9), ("y.rs", 12, 20)]),
        ];
        assert_eq!(stitchable(&groups), [0, 0, 1, 1, 1]);
    }

    /// Order-independence: the earlier region may belong to either group.
    #[test]
    fn stitchable_sees_gaps_in_both_directions() {
        let groups = [
            group(&[("a.rs", 10, 14), ("b.rs", 10, 14)]),
            // Site 1 sits BEFORE group 0's, site 2 after: gaps 1 and 1.
            group(&[("a.rs", 4, 9), ("b.rs", 15, 19)]),
        ];
        assert_eq!(stitchable(&groups), [1, 1, 1, 1, 1]);
    }
}
