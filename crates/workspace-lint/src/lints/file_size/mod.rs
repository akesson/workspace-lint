use globset::{GlobSet, GlobSetBuilder};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokei::{Config as TokeiConfig, Languages};

use crate::config::GlobPattern;
use crate::diagnostic::Diagnostic;
use crate::diagnostic::builder::at_file;
use crate::lints::shipped_source;
use crate::lints::{Lint, LintContext, LintId};

pub mod config;
#[cfg(test)]
mod tests;

pub(crate) use config::{FileSizeConfig, FileSizeRule};

pub(crate) struct FileSize {
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

impl Lint for FileSize {
    fn id(&self) -> LintId {
        LintId::FileSize
    }

    fn check(&self, _cx: &LintContext<'_>) -> Vec<Diagnostic> {
        check(&self.config)
    }
}

pub(crate) fn check(config: &FileSizeConfig) -> Vec<Diagnostic> {
    find_violations(&collect_file_lines(&config.rules), &config.rules)
}

/// Count code lines per file by running tokei from the process cwd.
///
/// Two counting regimes, picked per file:
/// - **`.rs` files matched by a rule glob** are counted as *shipped* source
///   (`shipped_source`): `#[cfg(test)]` items, `#[test]`/`#[wasm_bindgen_test]`
///   fns, and `tests/`/`benches/`/`examples/` dev-target trees are excluded, so
///   the budget matches `crate-size` to the line. A file that is itself an
///   out-of-line `#[cfg(test)] mod x;` target, or sits under a dev-target dir,
///   produces no entry (and therefore no diagnostic).
/// - **everything else** (non-Rust globs like `**/*.ts`, or `.rs` files no rule
///   targets) keeps tokei's raw whole-file count. Embedded languages (tokei
///   `children` reports — e.g. Rust inside a doc-code-fence) are summed into the
///   host file's key, matching the historical behavior for those globs.
///
/// Only glob-matched `.rs` files are read+parsed by the syn pass, so vendored
/// `corpus/` submodules and other untargeted trees are never parsed.
fn collect_file_lines(rules: &[FileSizeRule]) -> HashMap<String, usize> {
    let globset = rule_globset(rules);

    let mut languages = Languages::new();
    languages.get_statistics(&["."], &[], &TokeiConfig::default());

    let mut file_lines: HashMap<String, usize> = HashMap::new();
    // `.rs` files a rule targets: deferred to the shipped-source recount below.
    let mut rust_targets: Vec<(String, PathBuf)> = Vec::new();
    // A `.rs` file a rule targets is recounted as shipped source; everything
    // else keeps tokei's raw count (children summed into the host file's key).
    let mut route = |name: &Path, code: usize| {
        let rel = name.strip_prefix("./").unwrap_or(name);
        if is_rust(rel) && globset.is_match(rel) {
            rust_targets.push((rel.display().to_string(), rel.to_path_buf()));
        } else {
            *file_lines.entry(rel.display().to_string()).or_default() += code;
        }
    };
    for language in languages.values() {
        for report in &language.reports {
            route(&report.name, report.stats.code);
        }
        for child_reports in language.children.values() {
            for report in child_reports {
                route(&report.name, report.stats.code);
            }
        }
    }

    // Recount the targeted `.rs` files as shipped source. Dev-target files are
    // dropped here (file-size has no cargo metadata, so the owning crate root is
    // discovered by walking to the nearest `Cargo.toml`); out-of-line test-mod
    // files are dropped by `shipped_lines_by_file` itself.
    let to_count: Vec<PathBuf> = rust_targets
        .iter()
        .filter(|(_, path)| !shipped_source::in_dev_target_dir_rootless(path))
        .map(|(_, path)| path.clone())
        .collect();
    let shipped = shipped_source::shipped_lines_by_file(&to_count);
    for (rel, path) in rust_targets {
        if let Some(&code) = shipped.get(&path) {
            file_lines.insert(rel, code);
        }
    }
    file_lines
}

/// Build a matcher over every rule glob, to decide which files a rule targets.
fn rule_globset(rules: &[FileSizeRule]) -> GlobSet {
    let mut builder = GlobSetBuilder::new();
    for rule in rules {
        builder.add(rule.glob.compiled().clone());
    }
    builder.build().unwrap()
}

fn is_rust(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("rs")
}

/// Pure projection over per-file code-line counts: emit a diagnostic for every
/// file that exceeds a matching rule's `max_code_lines` (strict `>`). Kept
/// separate from the tokei walk so the glob-match + threshold + message logic
/// is unit-testable without the filesystem. Violations are sorted by path
/// within each rule for deterministic output.
fn find_violations(file_lines: &HashMap<String, usize>, rules: &[FileSizeRule]) -> Vec<Diagnostic> {
    let globset = rule_globset(rules);

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
                .note(format!(
                    "configured by [[file-size.rules]] glob = \"{}\"",
                    rule.glob.as_str()
                ))
                .build(),
            );
        }
    }

    diagnostics
}
