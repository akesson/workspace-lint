//! `duplicate-code` — name-invariant (Type-2) duplicate detection.
//!
//! Flags groups of structurally identical code regions — whole fns and
//! nested blocks — even when local variable names and literal values differ.
//! The matching engine (normalization + subtree hashing) lives in `detect`;
//! this module owns enumeration (which member files to scan, via the fast
//! tier's module walk) and diagnostic shaping.
//!
//! Advisory-only by design: there is no `MachineApplicable` fix. Resolving a
//! duplicate means *extracting* — choosing a name, parameterizing the
//! differing literals, picking a home — which is an author decision, so the
//! lint reports and the human refactors (or `expect!`s a deliberate copy).
//!
//! A clone group of N instances is emitted as N line-anchored diagnostics
//! (the `Diagnostic` model is single-span), each cross-referencing the other
//! sites in a note — so every site is independently `expect!`-suppressible
//! and the finding reads correctly from whichever copy you're looking at.

use std::collections::HashSet;
use std::path::PathBuf;

use globset::{GlobSet, GlobSetBuilder};

use crate::config::Globs;
use crate::{Lint, LintContext, LintId, Requirements};
use wl_diagnostic::Diagnostic;
use wl_diagnostic::builder::at_line;
use wl_engine::fast::{FastModel, TargetKind};

pub mod config;
mod detect;
#[cfg(test)]
mod tests;

pub use config::DuplicateCodeConfig;

use detect::{CloneGroup, Options, Region, ScanFile};

pub struct DuplicateCode {
    config: DuplicateCodeConfig,
}

impl DuplicateCode {
    pub fn new(config: DuplicateCodeConfig) -> Self {
        Self { config }
    }
}

impl Lint for DuplicateCode {
    fn id(&self) -> LintId {
        LintId::DuplicateCode
    }

    fn requirements(&self) -> Requirements {
        Requirements {
            needs_fast: true,
            needs_semantic: false,
        }
    }

    fn check(&self, cx: &LintContext<'_>) -> Vec<Diagnostic> {
        let fast = cx.fast.expect("duplicate-code declares needs_fast");
        let files = enumerate(fast, &self.config);
        let groups = detect::find_clones(&files, &options(&self.config));
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
    let include = globset(&config.include);
    let exclude = globset(&config.exclude);
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

fn globset(globs: &Globs) -> GlobSet {
    let mut builder = GlobSetBuilder::new();
    for glob in globs.iter() {
        builder.add(glob.compiled().clone());
    }
    builder
        .build()
        .expect("patterns were individually compiled at deserialize time")
}

/// One diagnostic per instance (each independently suppressible), sorted by
/// (file, line) across all groups.
fn emit(groups: &[CloneGroup]) -> Vec<Diagnostic> {
    let lint_id = LintId::DuplicateCode.id();
    let mut diagnostics = Vec::new();
    for group in groups {
        for (i, inst) in group.instances.iter().enumerate() {
            let lines = inst.line_end - inst.line_start + 1;
            diagnostics.push(
                at_line(
                    lint_id,
                    format!(
                        "duplicated code: {} structurally identical instances (~{lines} lines)",
                        group.instances.len(),
                    ),
                    inst.file.clone(),
                    inst.line_start,
                )
                .note(format!("also found at: {}", other_sites(group, i)))
                .note("matching ignores local variable names and literal values")
                .help("extract the shared logic into one function the copies can call")
                .build(),
            );
        }
    }
    diagnostics.sort_by(|a, b| {
        let key = |d: &Diagnostic| {
            d.primary
                .as_ref()
                .map(|p| (p.file.clone(), p.line_start))
                .unwrap_or_default()
        };
        key(a).cmp(&key(b))
    });
    diagnostics
}

/// `file:line` list of the group's other instances, capped so a pathological
/// group doesn't turn the note into a wall.
fn other_sites(group: &CloneGroup, current: usize) -> String {
    const MAX_LISTED: usize = 5;
    let others: Vec<&Region> = group
        .instances
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != current)
        .map(|(_, r)| r)
        .collect();
    let mut listed: Vec<String> = others
        .iter()
        .take(MAX_LISTED)
        .map(|r| format!("{}:{}", r.file.display(), r.line_start))
        .collect();
    if others.len() > MAX_LISTED {
        listed.push(format!("and {} more", others.len() - MAX_LISTED));
    }
    listed.join(", ")
}
