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

pub mod config;

pub use config::DuplicateCodeConfig;

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
    const REQUIRES: Requirements = Requirements {
        needs_fast: true,
        needs_semantic: false,
    };

    fn run(&self, cx: &LintContext<'_>) -> Vec<Diagnostic> {
        let files = enumerate(cx.fast_model(Self::ID), &self.config);
        let groups = find_clones(&files, &options(&self.config));
        emit(&groups)
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
fn emit(groups: &[CloneGroup]) -> Vec<Diagnostic> {
    let lint_id = LintId::DuplicateCode.id();
    groups
        .iter()
        .map(|group| {
            let anchor = &group.instances[0];
            let lines = anchor.line_end - anchor.line_start + 1;
            at_line(
                lint_id,
                format!(
                    "duplicated code: {} structurally identical instances (~{lines} lines)",
                    group.instances.len(),
                ),
                anchor.file.clone(),
                anchor.line_start,
            )
            .note(format!("also found at: {}", other_sites(group)))
            .note("matching ignores local variable names and literal values")
            .help("extract the shared logic into one function the copies can call")
            .build()
        })
        .collect()
}

/// `file:line` list of the group's instances beyond the anchor, capped so a
/// pathological group doesn't turn the note into a wall.
fn other_sites(group: &CloneGroup) -> String {
    const MAX_LISTED: usize = 5;
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
