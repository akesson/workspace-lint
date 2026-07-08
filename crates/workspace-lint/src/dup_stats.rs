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
        "{:<56} {:>4} {:>5} {:>6} {:>5} {:>8} {:>5} {:>6} {:>5}",
        "anchor", "inst", "kind", "tokens", "lines", "literals", "divg", "params", "drift"
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
        let _ = writeln!(
            out,
            "{anchor:<56} {:>4} {:>5} {:>6} {:>5} {literals:>8} {divg:>5} {params:>6} {drift:>5}",
            g.instances,
            kind_name(g.kind),
            g.tokens,
            g.lines,
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
    let count = |k: CandidateKind| report.groups.iter().filter(|g| g.kind == k).count();
    let _ = writeln!(
        out,
        "kind distribution: fn:{} block:{} run:{}",
        count(CandidateKind::Fn),
        count(CandidateKind::Block),
        count(CandidateKind::Run),
    );
    out
}

fn kind_name(kind: CandidateKind) -> &'static str {
    match kind {
        CandidateKind::Fn => "fn",
        CandidateKind::Block => "block",
        CandidateKind::Run => "run",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use wl_lints::duplicate_code::{GroupMeasure, MeasureReport};

    /// The readout carries every section the calibration protocol reads,
    /// and a `None` divergence renders as dashes, not zeros.
    #[test]
    fn render_covers_all_sections() {
        let report = MeasureReport {
            groups: vec![GroupMeasure {
                file: PathBuf::from("crates/x/src/a.rs"),
                line: 10,
                instances: 2,
                kind: CandidateKind::Fn,
                tokens: 60,
                lines: 9,
                divergence: None,
            }],
            stitchable: [0, 1, 1, 2, 2],
        };
        let text = render(&report);
        assert!(text.contains("duplicate-code stats: 1 groups"));
        assert!(text.contains("crates/x/src/a.rs:10"));
        assert!(text.contains(" -"), "None divergence renders as dashes");
        assert!(text.contains("params histogram:"));
        assert!(text.contains("suppressed at max-parameters"));
        assert!(text.contains("drift candidates: 0"));
        assert!(text.contains("stitchable group pairs at gap <=1:0 <=2:1"));
        assert!(text.contains("kind distribution: fn:1 block:0 run:0"));
    }
}
