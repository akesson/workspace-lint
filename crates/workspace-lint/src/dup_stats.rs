//! Renders `check duplicate-code --stats`: the measure-only readout the
//! duplicate-code thresholds are tuned against. Plain text on stdout, one
//! row per group plus the summaries the calibration protocol reads
//! (parameter histogram, would-be suppression counts, drift candidates,
//! stitchable adjacency, candidate-kind distribution).

use std::fmt::Write;

use wl_diagnostic::render::display_path;
use wl_engine::fast::clones::CandidateKind;
use wl_lints::duplicate_code::MeasureReport;

pub(crate) fn render(report: &MeasureReport) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "duplicate-code stats: {} groups", report.groups.len());
    let _ = writeln!(
        out,
        "{:<56} {:>4} {:>5} {:>6} {:>5} {:>8} {:>5} {:>6} {:>5} {:>5} {:>16} {:<23}",
        "anchor",
        "inst",
        "kind",
        "tokens",
        "lines",
        "literals",
        "divg",
        "params",
        "drift",
        "lvout",
        "fp",
        "class",
    );
    for g in &report.groups {
        let anchor = format!("{}:{}", display_path(&g.file), g.line);
        let (literals, divg, params, drift) = match &g.divergence {
            Some(d) => (
                d.positions.to_string(),
                d.divergent.to_string(),
                d.params.to_string(),
                d.violations.len().to_string(),
            ),
            None => ("-".into(), "-".into(), "-".into(), "-".into()),
        };
        let lvout = match &g.liveness {
            Some(l) => l.max_live_out.to_string(),
            None => "-".into(),
        };
        let fp = format!("{:016x}", g.fingerprint);
        let _ = writeln!(
            out,
            "{anchor:<56} {:>4} {:>5} {:>6} {:>5} {literals:>8} {divg:>5} {params:>6} {drift:>5} {lvout:>5} {fp:>16} {:<23}",
            g.instances,
            g.kind.label(),
            g.tokens,
            g.lines,
            g.class,
        );
    }

    let with_divergence: Vec<_> = report
        .groups
        .iter()
        .filter_map(|g| g.divergence.as_ref())
        .collect();
    let mut histogram: Vec<(usize, usize)> = Vec::new();
    for d in &with_divergence {
        match histogram.iter_mut().find(|(p, _)| *p == d.params) {
            Some((_, n)) => *n += 1,
            None => histogram.push((d.params, 1)),
        }
    }
    histogram.sort_unstable();
    let _ = write!(out, "\nparams histogram:");
    for (params, n) in &histogram {
        let _ = write!(out, " {params}:{n}");
    }
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "literal-identical groups (params = 0): {}/{}",
        with_divergence.iter().filter(|d| d.params == 0).count(),
        with_divergence.len(),
    );
    let _ = write!(out, "suppressed at max-parameters");
    for threshold in 2..=5 {
        let n = with_divergence
            .iter()
            .filter(|d| d.violations.is_empty() && d.params > threshold)
            .count();
        let _ = write!(out, " {threshold}:{n}");
    }
    let _ = writeln!(out);

    let drifted: Vec<_> = report
        .groups
        .iter()
        .filter_map(|g| g.divergence.as_ref())
        .flat_map(|d| &d.violations)
        .collect();
    let _ = writeln!(out, "drift candidates: {}", drifted.len());
    for v in drifted {
        let _ = writeln!(
            out,
            "  {}:{}: {} where the mapping elsewhere expects {}",
            display_path(&v.file),
            v.line,
            v.found,
            v.expected
        );
    }

    let _ = write!(out, "stitchable group pairs at gap");
    for (i, n) in report.stitchable.iter().enumerate() {
        let _ = write!(out, " <={}:{n}", i + 1);
    }
    let _ = writeln!(out);

    // Live-out distribution over statement-run groups (the only kind with a
    // liveness signature): how many values each would return if extracted —
    // the input to the max-live-out downgrade.
    let with_liveness: Vec<_> = report
        .groups
        .iter()
        .filter_map(|g| g.liveness.as_ref())
        .collect();
    let mut lv_histogram: Vec<(usize, usize)> = Vec::new();
    for l in &with_liveness {
        match lv_histogram
            .iter_mut()
            .find(|(lo, _)| *lo == l.max_live_out)
        {
            Some((_, n)) => *n += 1,
            None => lv_histogram.push((l.max_live_out, 1)),
        }
    }
    lv_histogram.sort_unstable();
    let _ = write!(out, "live-out histogram (runs):");
    for (lo, n) in &lv_histogram {
        let _ = write!(out, " {lo}:{n}");
    }
    let _ = writeln!(out);

    let count = |k: CandidateKind| report.groups.iter().filter(|g| g.kind == k).count();
    let _ = writeln!(
        out,
        "kind distribution: fn:{} block:{} run:{}",
        count(CandidateKind::Fn),
        count(CandidateKind::Block),
        count(CandidateKind::Run),
    );

    // Syntactic only: the merge family (merge-identical-fns / delete-dead-copy
    // / merge-withheld) is a call-graph verdict the build-free readout can't
    // reach — those groups show as their fallback syntactic class here.
    let mut class_counts: Vec<(&str, usize)> = Vec::new();
    for g in &report.groups {
        match class_counts.iter_mut().find(|(c, _)| *c == g.class) {
            Some((_, n)) => *n += 1,
            None => class_counts.push((g.class, 1)),
        }
    }
    class_counts.sort_unstable();
    let _ = write!(out, "class distribution (syntactic):");
    for (c, n) in &class_counts {
        let _ = write!(out, " {c}:{n}");
    }
    let _ = writeln!(out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use wl_engine::fast::clones::liveness::Liveness;
    use wl_lints::duplicate_code::{GroupMeasure, MeasureReport};

    /// The readout carries every section the calibration protocol reads, a
    /// `None` divergence/liveness renders as dashes (not zeros), and a run
    /// group's live-out count feeds both the `lvout` column and the histogram.
    #[test]
    fn render_covers_all_sections() {
        let report = MeasureReport {
            groups: vec![
                GroupMeasure {
                    fingerprint: 0x00ab_cdef_0000_0001,
                    file: PathBuf::from("crates/x/src/a.rs"),
                    line: 10,
                    instances: 2,
                    kind: CandidateKind::Fn,
                    tokens: 60,
                    lines: 9,
                    divergence: None,
                    liveness: None,
                    class: "ui-component",
                },
                GroupMeasure {
                    fingerprint: 0x00ab_cdef_0000_0002,
                    file: PathBuf::from("crates/x/src/b.rs"),
                    line: 20,
                    instances: 2,
                    kind: CandidateKind::Run,
                    tokens: 50,
                    lines: 8,
                    divergence: None,
                    liveness: Some(Liveness {
                        live_in: vec!["items".into(), "config".into()],
                        live_out: vec!["count".into(), "total".into()],
                        max_live_out: 2,
                    }),
                    class: "unclassified",
                },
            ],
            stitchable: [0, 1, 1, 2, 2],
        };
        let text = render(&report);
        assert!(text.contains("duplicate-code stats: 2 groups"));
        assert!(text.contains("crates/x/src/a.rs:10"));
        assert!(text.contains(" -"), "None divergence renders as dashes");
        assert!(text.contains("lvout"), "the live-out column header renders");
        assert!(
            text.contains("00abcdef00000001"),
            "the fingerprint column renders the baseline match key"
        );
        assert!(text.contains("params histogram:"));
        assert!(text.contains("suppressed at max-parameters"));
        assert!(text.contains("drift candidates: 0"));
        assert!(text.contains("stitchable group pairs at gap <=1:0 <=2:1"));
        assert!(text.contains("live-out histogram (runs): 2:1"));
        assert!(text.contains("kind distribution: fn:1 block:0 run:1"));
        assert!(text.contains("ui-component"), "the class column renders");
        assert!(text.contains("class distribution (syntactic): ui-component:1"));
    }
}
