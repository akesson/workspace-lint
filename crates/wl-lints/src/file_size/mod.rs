use globset::GlobSet;
use std::collections::HashMap;
use std::path::PathBuf;

use wl_diagnostic::Diagnostic;
use wl_diagnostic::builder::at_file;
use wl_engine::fast::FastModel;
use wl_lint_api::config::{GlobPattern, glob_set};
use wl_lint_api::util::rule_glob_note;
use wl_lint_api::{LintContext, LintId, LintImpl, Requirements};

pub mod config;
#[cfg(test)]
mod tests;

pub use config::{FileSizeConfig, FileSizeRule};

pub struct FileSize {
    config: FileSizeConfig,
}

impl FileSize {
    pub fn new(config: FileSizeConfig) -> Self {
        Self { config }
    }

    pub fn from_cli(glob: String, max_code_lines: usize) -> Self {
        Self::new(FileSizeConfig {
            rules: vec![FileSizeRule {
                glob: GlobPattern::from_cli(&glob),
                max_code_lines,
            }],
        })
    }
}

impl LintImpl for FileSize {
    const ID: LintId = LintId::FileSize;
    const DOC: &'static str = include_str!("DOC.md");
    // The shared source-measurement sweep lives on the FastModel — this is
    // what makes the lint workspace-rooted (cwd-invariant) and lets it share
    // one tokei walk with crate-size.
    const REQUIRES: Requirements = Requirements {
        needs_fast: true,
        needs_semantic: false,
    };

    fn run(&self, cx: &LintContext<'_>) -> Vec<Diagnostic> {
        check(&self.config, cx.fast_model(Self::ID))
    }
}

pub(crate) fn check(config: &FileSizeConfig, fast: &FastModel) -> Vec<Diagnostic> {
    let globset = rule_globset(&config.rules);
    find_violations(&collect_file_lines(fast, &globset), &config.rules, &globset)
}

/// Per-file code-line counts from the FastModel's shared measurement sweep,
/// keyed by workspace-root-relative path.
///
/// Two counting regimes, picked per file:
/// - **`.rs` files matched by a rule glob** are counted as *shipped* source
///   (`shipped_source`): `#[cfg(test)]` items, `#[test]`/`#[wasm_bindgen_test]`
///   fns, and `tests/`/`benches/`/`examples/` dev-target trees are excluded, so
///   the budget matches `crate-size` to the line. A file that is itself an
///   out-of-line `#[cfg(test)] mod x;` target, or sits under a dev-target dir,
///   produces no entry (and therefore no diagnostic).
/// - **everything else** (non-Rust globs like `**/*.ts`, or `.rs` files no rule
///   targets) keeps tokei's raw whole-file count, embedded languages (a Rust
///   doc-code-fence in Markdown) summed into the host file's key.
///
/// Only glob-matched `.rs` files are read+parsed by the syn pass, so vendored
/// `corpus/` submodules and other untargeted trees are never parsed.
fn collect_file_lines(fast: &FastModel, globset: &GlobSet) -> HashMap<String, usize> {
    let measure = fast.source_measure();

    let mut file_lines: HashMap<String, usize> = HashMap::new();
    // `.rs` files a rule targets: deferred to the shipped-source recount below.
    let mut rust_targets: Vec<(String, PathBuf)> = Vec::new();
    for f in measure.files() {
        // An out-of-root member's files have no workspace-relative spelling;
        // the old cwd-rooted walk never saw them either — scope preserved.
        let Some(rel) = &f.rel else { continue };
        if f.is_rust() && globset.is_match(rel) {
            if !measure.in_dev_target(&f.abs) {
                rust_targets.push((rel.display().to_string(), f.abs.clone()));
            }
        } else {
            *file_lines.entry(rel.display().to_string()).or_default() += f.code + f.embedded;
        }
    }

    // Recount the targeted `.rs` files as shipped source; out-of-line
    // test-mod files are dropped by `shipped_rust_lines` itself.
    let to_count: Vec<PathBuf> = rust_targets.iter().map(|(_, abs)| abs.clone()).collect();
    let shipped = measure.shipped_rust_lines(&to_count);
    for (rel, abs) in rust_targets {
        if let Some(&code) = shipped.get(&abs) {
            file_lines.insert(rel, code);
        }
    }
    file_lines
}

/// Build a matcher over every rule glob, to decide which files a rule targets.
fn rule_globset(rules: &[FileSizeRule]) -> GlobSet {
    glob_set(rules.iter().map(|r| &r.glob)).unwrap_or_default()
}

/// Pure projection over per-file code-line counts: emit a diagnostic for every
/// file that exceeds a matching rule's `max_code_lines` (strict `>`). Kept
/// separate from the measurement sweep so the glob-match + threshold + message
/// logic is unit-testable without the filesystem. Violations are sorted by
/// path within each rule for deterministic output.
fn find_violations(
    file_lines: &HashMap<String, usize>,
    rules: &[FileSizeRule],
    globset: &GlobSet,
) -> Vec<Diagnostic> {
    let mut violations: Vec<Vec<(&String, usize)>> = vec![Vec::new(); rules.len()];
    for (path_str, code_lines) in file_lines {
        let path = std::path::Path::new(path_str);
        for &rule_idx in &globset.matches(path) {
            if *code_lines > rules[rule_idx].max_code_lines {
                violations[rule_idx].push((path_str, *code_lines));
            }
        }
    }

    let lint_id = LintId::FileSize.id();
    let mut diagnostics = Vec::new();
    for (rule_idx, mut viols) in violations.into_iter().enumerate() {
        viols.sort_by(|a, b| a.0.cmp(b.0));
        let rule = &rules[rule_idx];
        for (path, code_lines) in viols {
            diagnostics.push(
                at_file(
                    lint_id,
                    format!(
                        "file exceeds {} code lines ({code_lines})",
                        rule.max_code_lines
                    ),
                    path.clone(),
                )
                .help("split this file into focused submodules (e.g. a `foo/` directory with a `mod.rs`)")
                .help("extract related structs, enums, or trait impls into their own modules")
                .help("only shipped source counts — `#[cfg(test)]` and `#[test]` code is already excluded")
                // Rule attribution is identical for every file the rule
                // matches — once per run is enough.
                .note_once(rule_glob_note(LintId::FileSize, &rule.glob))
                .build(),
            );
        }
    }

    diagnostics
}
