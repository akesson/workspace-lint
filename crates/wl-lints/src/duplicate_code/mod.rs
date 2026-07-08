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
        groups
            .iter()
            .filter_map(|group| {
                let divergence = analyzer.as_mut().and_then(|a| a.analyze(group));
                emit(
                    group,
                    divergence.as_ref(),
                    self.config.max_parameters,
                    classifier.as_mut(),
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
fn emit(
    group: &CloneGroup,
    divergence: Option<&Divergence>,
    max_parameters: usize,
    classifier: Option<&mut Classifier>,
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
    // The class note is appended after the divergence note (pinned order).
    if let Some(note) = class.as_ref().and_then(RefactoringClass::note) {
        builder = builder.note(note);
    }
    Some(builder.build())
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
