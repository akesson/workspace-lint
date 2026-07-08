//! `duplicate-code` — name-invariant (Type-2) duplicate detection.
//!
//! Flags groups of structurally identical code regions — whole fns and
//! nested blocks — even when local variable names and literal values differ.
//! The matching engine (normalization + subtree hashing) is the fast tier's
//! [`wl_engine::fast::clones`] scanner; this module owns enumeration (which
//! member files to scan, via the fast tier's module walk) and diagnostic
//! shaping.
//!
//! Advisory-only by design: there is no `MachineApplicable` fix. Resolving a
//! duplicate means *extracting* — choosing a name, parameterizing the
//! differing literals, picking a home — which is an author decision, so the
//! lint reports and the human refactors (or `expect!`s a deliberate copy).
//!
//! A clone group is emitted as ONE line-anchored diagnostic at its first
//! instance (by file, line), with every other site cross-referenced in a
//! note — N mirrored warnings per group read as the tool repeating itself.
//! The trade: silencing a deliberate duplication takes a single `expect!` at
//! the anchor site, and the other sites have no directive anchor of their own.

use std::collections::HashSet;
use std::path::PathBuf;

use wl_diagnostic::Diagnostic;
use wl_diagnostic::builder::at_line;
use wl_engine::fast::{FastModel, TargetKind};
use wl_lint_api::{LintContext, LintId, LintImpl, Requirements};

mod classify;
pub mod config;
mod measure;

use classify::{Classifier, RefactoringClass};
pub use config::DuplicateCodeConfig;
pub use measure::{GroupMeasure, MeasureReport, measure};

use wl_engine::fast::clones::divergence::{Divergence, DivergenceAnalyzer};
use wl_engine::fast::clones::liveness::{Liveness, LivenessAnalyzer};
use wl_engine::fast::clones::{CloneGroup, Options, Region, ScanFile, find_clones};

pub struct DuplicateCode {
    config: DuplicateCodeConfig,
}

impl DuplicateCode {
    pub fn new(config: DuplicateCodeConfig) -> Self {
        Self { config }
    }
}

impl LintImpl for DuplicateCode {
    const ID: LintId = LintId::DuplicateCode;
    // Semantic: the classifier's merge family confirms two "identical" fns are
    // interchangeable against the rustc call graph (an IR-only fact). The lint
    // is either skipped (`--fast-only`) or runs at full accuracy — never a
    // degraded variant. `needs_fast` too: enumeration + detection are fast-tier.
    const REQUIRES: Requirements = Requirements {
        needs_fast: true,
        needs_semantic: true,
    };

    fn run(&self, cx: &LintContext<'_>) -> Vec<Diagnostic> {
        // A memberless workspace has no semantic tier — nothing to classify or
        // (having no members) scan; bail as the other semantic lints do.
        let Some((fast, semantic)) = cx.semantic_models(Self::ID) else {
            return Vec::new();
        };
        let files = enumerate(fast, &self.config);
        let groups = find_clones(&files, &options(&self.config));
        // Divergence only reads meaningfully when literals were abstracted;
        // under `ignore-literals = false` instances are literal-identical by
        // construction and every group ships unshaped.
        let mut analyzer = self
            .config
            .ignore_literals
            .then(|| DivergenceAnalyzer::new(&files));
        // The classifier names each group's fix; disabled, groups keep the
        // generic help. It consults the call graph for the merge family only.
        let mut classifier = self
            .config
            .classify
            .then(|| Classifier::new(&files, Some(semantic), &self.config.component_macros));
        // Lazy and run-gated internally, so it is built unconditionally: a
        // group that is not a statement run resolves no liveness and pays
        // nothing.
        let mut liveness = LivenessAnalyzer::new(&files);
        groups
            .iter()
            .filter_map(|group| {
                let divergence = analyzer.as_mut().and_then(|a| a.analyze(group));
                emit(
                    group,
                    divergence.as_ref(),
                    self.config.max_parameters,
                    classifier.as_mut(),
                    &mut liveness,
                    self.config.max_live_out,
                )
            })
            .collect()
    }
}

fn options(config: &DuplicateCodeConfig) -> Options {
    Options {
        min_lines: config.min_lines,
        min_tokens: config.min_tokens,
        min_instances: config.min_instances,
        ignore_literals: config.ignore_literals,
        ignore_test_code: config.ignore_test_code,
        cross_crate_only: config.cross_crate_only,
        min_distinct_anchors: config.min_distinct_anchors,
        min_non_repeating_ratio: config.min_non_repeating_ratio,
    }
}

/// Parse every member source file the config targets, once. The module walk
/// (not a filesystem glob) is the enumeration surface: it is member-scoped,
/// target-aware (so `ignore-test-code` can drop whole dev targets), and only
/// reaches files that are part of some module tree — orphans and vendored
/// trees are never parsed. Files are deduped by canonical path because inline
/// `mod` blocks share their parent's file and a file can be reached from
/// several targets (`lib.rs` + `main.rs` sharing modules via `#[path]`).
fn enumerate(fast: &FastModel, config: &DuplicateCodeConfig) -> Vec<ScanFile> {
    let include = config.include.glob_set();
    let exclude = config.exclude.glob_set();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut out = Vec::new();
    for krate in fast.members() {
        for target in krate.targets() {
            if config.ignore_test_code
                && matches!(
                    target.kind,
                    TargetKind::Test | TargetKind::Bench | TargetKind::Example
                )
            {
                continue;
            }
            for module in target.root.walk() {
                let canonical = module
                    .file
                    .canonicalize()
                    .unwrap_or_else(|_| module.file.clone());
                if !seen.insert(canonical) {
                    continue;
                }
                let rel = fast.crate_relative_path(&module.file);
                if !config.include.0.is_empty() && !include.is_match(&rel) {
                    continue;
                }
                if exclude.is_match(&rel) {
                    continue;
                }
                // A file syn can't parse is a compile error some other
                // surface reports; nothing to scan here.
                let Ok(ast) = fast.parse_file(&module.file) else {
                    continue;
                };
                out.push(ScanFile {
                    rel_path: rel,
                    krate: krate.name.clone(),
                    ast,
                });
            }
        }
    }
    out
}

/// One diagnostic per group, anchored at the group's first instance — groups
/// arrive from `find_clones` already sorted by that anchor's (file, line).
/// The group's literal divergence decides its fate and its shaping:
/// - a drift violation keeps the group unconditionally (a probable
///   copy-paste bug must never be silenced by the table gate) and is
///   rendered as its own note per defecting site;
/// - otherwise `params > max-parameters` drops the group — that many
///   independently-varying literals is a data table, not copy-paste;
/// - otherwise one actionability note: literal-identical instances (the
///   most mechanical extraction) or the parameter count extracting would
///   take.
///
/// A statement-run group additionally carries an extraction-signature note
/// (its live-in parameters and live-out return values) and, when it would
/// return more than `max_live_out` values, is downgraded to `warn` via a
/// lint-chosen level — an advisory softening that is deliberately kept out of
/// the classifier so its "classification never changes level" contract holds.
/// The run's live parameters are variables; the divergence note's are literals,
/// so the two notes co-exist and a full extraction would take the sum.
fn emit(
    group: &CloneGroup,
    divergence: Option<&Divergence>,
    max_parameters: usize,
    classifier: Option<&mut Classifier>,
    liveness: &mut LivenessAnalyzer<'_>,
    max_live_out: usize,
) -> Option<Diagnostic> {
    let anchor = &group.instances[0];
    let lines = anchor.line_end - anchor.line_start + 1;
    // Gate BEFORE classifying so a suppressed data table never pays the
    // classifier: a drift violation keeps the group unconditionally, otherwise
    // `params > max-parameters` drops it.
    if let Some(d) = divergence
        && d.violations.is_empty()
        && max_parameters > 0
        && d.params > max_parameters
    {
        return None;
    }
    // Classify the survivor. `identical` — differing at most in local names —
    // gates the merge family (no divergence ⇒ exact-literals mode ⇒ identical).
    let class = classifier.map(|c| {
        let identical = divergence.is_none_or(|d| d.params == 0 && d.violations.is_empty());
        c.classify(group, identical)
    });
    let help = match &class {
        Some(c) => c.help(),
        None => classify::GENERIC_HELP.to_string(),
    };
    let mut builder = at_line(
        LintId::DuplicateCode.id(),
        format!(
            "duplicated code: {} structurally identical instances (~{lines} lines)",
            group.instances.len(),
        ),
        anchor.file.clone(),
        anchor.line_start,
    )
    .note(format!("also found at: {}", other_sites(group)))
    .note("matching ignores local variable names and literal values")
    .help(help);
    if let Some(d) = divergence {
        for v in d.violations.iter().take(MAX_LISTED) {
            builder = builder.note(format!(
                "possible copy-paste drift: {}:{} has {} where the mapping elsewhere expects {}",
                wl_diagnostic::render::display_path(&v.file),
                v.line,
                v.found,
                v.expected,
            ));
        }
        if d.violations.is_empty() {
            builder = builder.note(match d.params {
                0 => "instances are identical (differing at most in local names)".to_string(),
                1 => "extracting would take ~1 parameter for the differing literals".to_string(),
                n => format!("extracting would take ~{n} parameters for the differing literals"),
            });
        }
    }
    // A statement-run group prices its extraction as a signature note, and a
    // multi-return extraction (live-out beyond the threshold) downgrades to a
    // lint-chosen `warn` so it stops failing CI without vanishing.
    if let Some(lv) = liveness.analyze(group) {
        builder = builder.note(extraction_signature_note(&lv));
        if max_live_out > 0 && lv.max_live_out > max_live_out {
            builder = builder.level_explicit(wl_diagnostic::Level::Warn);
        }
    }
    // The class note is appended after the divergence note (pinned order).
    if let Some(note) = class.as_ref().and_then(RefactoringClass::note) {
        builder = builder.note(note);
    }
    Some(builder.build())
}

/// The extraction-signature note for a statement-run group: the parameters an
/// extraction would take (live-in) and the values it would return (live-out),
/// named from the source. More than one return value reads as awkward and
/// pairs with the group's downgrade.
fn extraction_signature_note(lv: &Liveness) -> String {
    let take = match lv.live_in.len() {
        0 => "take no parameters".to_string(),
        1 => format!("take 1 parameter ({})", name_list(&lv.live_in)),
        n => format!("take {n} parameters ({})", name_list(&lv.live_in)),
    };
    let ret = match lv.max_live_out {
        0 => "and return nothing".to_string(),
        1 => format!("and return {}", name_list(&lv.live_out)),
        n => format!(
            "but needs {n} return values ({}) — extraction is awkward; consider restructuring",
            name_list(&lv.live_out),
        ),
    };
    format!("an extracted fn would {take} {ret}")
}

/// A comma-joined name list, capped at [`MAX_LISTED`] with an `and N more`
/// tail (matching the diagnostic's other site-list caps).
fn name_list(names: &[String]) -> String {
    let mut listed: Vec<String> = names.iter().take(MAX_LISTED).cloned().collect();
    if names.len() > MAX_LISTED {
        listed.push(format!("and {} more", names.len() - MAX_LISTED));
    }
    listed.join(", ")
}

/// Cap for per-diagnostic site lists (cross-references and drift notes), so
/// a pathological group doesn't turn the notes into a wall.
const MAX_LISTED: usize = 5;

/// `file:line` list of the group's instances beyond the anchor, capped at
/// [`MAX_LISTED`].
fn other_sites(group: &CloneGroup) -> String {
    let others: &[Region] = &group.instances[1..];
    let mut listed: Vec<String> = others
        .iter()
        .take(MAX_LISTED)
        .map(|r| {
            // `display_path` forces forward slashes so the note text matches
            // the renderer's span paths on Windows.
            format!(
                "{}:{}",
                wl_diagnostic::render::display_path(&r.file),
                r.line_start
            )
        })
        .collect();
    if others.len() > MAX_LISTED {
        listed.push(format!("and {} more", others.len() - MAX_LISTED));
    }
    listed.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn liveness(live_in: &[&str], live_out: &[&str], max_live_out: usize) -> Liveness {
        Liveness {
            live_in: live_in.iter().map(|s| s.to_string()).collect(),
            live_out: live_out.iter().map(|s| s.to_string()).collect(),
            max_live_out,
        }
    }

    /// The full parameter/return phrasing matrix, including the zero and
    /// singular/plural edges and the awkward multi-return clause.
    #[test]
    fn extraction_signature_note_matrix() {
        assert_eq!(
            extraction_signature_note(&liveness(&["items", "config"], &["total"], 1)),
            "an extracted fn would take 2 parameters (items, config) and return total",
        );
        assert_eq!(
            extraction_signature_note(&liveness(&["items"], &[], 0)),
            "an extracted fn would take 1 parameter (items) and return nothing",
        );
        assert_eq!(
            extraction_signature_note(&liveness(&[], &[], 0)),
            "an extracted fn would take no parameters and return nothing",
        );
        assert_eq!(
            extraction_signature_note(&liveness(&[], &["total"], 1)),
            "an extracted fn would take no parameters and return total",
        );
        assert_eq!(
            extraction_signature_note(&liveness(&["items"], &["count", "total"], 2)),
            "an extracted fn would take 1 parameter (items) but needs 2 return values \
             (count, total) — extraction is awkward; consider restructuring",
        );
    }

    /// Beyond `MAX_LISTED` names, the list caps with an `and N more` tail.
    #[test]
    fn name_list_caps_long_lists() {
        let many: Vec<String> = (0..7).map(|i| format!("v{i}")).collect();
        assert_eq!(name_list(&many), "v0, v1, v2, v3, v4, and 2 more");
    }
}
