use std::path::{Path, PathBuf};
use tokei::{Config as TokeiConfig, Languages};
use wl_engine::fast::FastModel;

use wl_diagnostic::Diagnostic;
use wl_diagnostic::builder::at_crate;
use wl_lint_api::config::{GlobPattern, glob_set};
use wl_lint_api::{LintContext, LintId, LintImpl, Requirements};

pub mod config;
#[cfg(test)]
mod tests;

use crate::shipped_source;

pub use config::{CrateSizeConfig, CrateSizeRule};

pub struct CrateSize {
    config: CrateSizeConfig,
}

impl CrateSize {
    pub fn new(config: CrateSizeConfig) -> Self {
        Self { config }
    }

    pub fn from_cli(glob: String, max_code_lines: usize, include: Vec<String>) -> Self {
        Self::new(CrateSizeConfig {
            rules: vec![CrateSizeRule {
                glob: GlobPattern::from_cli(&glob),
                max_code_lines,
                include: if include.is_empty() {
                    None
                } else {
                    Some(include.iter().map(|p| GlobPattern::from_cli(p)).collect())
                },
            }],
        })
    }
}

impl LintImpl for CrateSize {
    const ID: LintId = LintId::CrateSize;
    const REQUIRES: Requirements = Requirements {
        needs_fast: true,
        needs_semantic: false,
    };

    fn run(&self, cx: &LintContext<'_>) -> Vec<Diagnostic> {
        check(&self.config, cx.fast_model(Self::ID))
    }
}

pub(crate) fn check(config: &CrateSizeConfig, fast: &FastModel) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    for rule in &config.rules {
        let matcher = rule.glob.compiled().compile_matcher();

        // A crate-size budget is about *shipped code*, not committed data and
        // not tests: JSON oracle snapshots and TOML fixture manifests under a
        // crate dir, the `tests/` / `benches/` / `examples/` dev-target trees,
        // and in-file `#[cfg(test)]` items all sit outside the budget (see
        // `shipped_source`). So only Rust source (`*.rs`) is counted by default.
        // Override per rule with `include` to count other file types or to
        // narrow to specific Rust files. Patterns match the file name (not the
        // full path).
        let rust_only;
        let include_patterns = match &rule.include {
            Some(patterns) => patterns.as_slice(),
            None => {
                rust_only = [GlobPattern::new("*.rs").expect("`*.rs` is a valid glob")];
                &rust_only
            }
        };
        let include_set = glob_set(include_patterns).unwrap_or_default();

        // Every workspace member whose workspace-relative manifest directory
        // matches the rule's glob (`members_matching`): the lint can only
        // target real cargo workspace members (cargo's `members`/`exclude`/
        // glob semantics are honored), and the resulting anchor path is
        // workspace-relative for free — `# workspace-lint: allow(crate-size)`
        // in a member Cargo.toml matches.
        let crate_totals: Vec<(String, usize)> = fast
            .members_matching(|key| matcher.is_match(key))
            .into_iter()
            .map(|(rel, krate)| (rel, count_crate_code(&krate.manifest_dir, &include_set)))
            .collect();

        diagnostics.extend(find_crate_violations(rule, &crate_totals));
    }

    diagnostics
}

/// Sum the `*.rs` (or `include`-filtered) *shipped* code lines under a member's
/// on-disk directory. The absolute path is passed so tokei's walk doesn't depend
/// on the process cwd. `include` patterns match the file *name* only (not the
/// path). Test code is excluded: dev-target dirs wholesale, and in-file test
/// items per `shipped_source`. Non-Rust includes (e.g. `*.json`) keep tokei's
/// own count — syn can't parse them, and they carry no test items to remove.
fn count_crate_code(abs: &Path, include_set: &globset::GlobSet) -> usize {
    let mut languages = Languages::new();
    languages.get_statistics(
        &[abs.display().to_string().as_str()],
        &[],
        &TokeiConfig::default(),
    );

    let mut rust_files: Vec<PathBuf> = Vec::new();
    let mut other_code = 0;
    for language in languages.values() {
        for report in &language.reports {
            let name = report.name.file_name().unwrap_or_default();
            if !include_set.is_match(Path::new(name)) {
                continue;
            }
            if shipped_source::in_dev_target_dir(abs, &report.name) {
                continue;
            }
            if report.name.extension().and_then(|e| e.to_str()) == Some("rs") {
                rust_files.push(report.name.clone());
            } else {
                other_code += report.stats.code;
            }
        }
    }
    other_code + shipped_source::count_rust_shipped(&rust_files)
}

/// Pure projection: emit a diagnostic for each `(crate_relative_dir, total)`
/// over the rule's `max_code_lines` (strict `>`). Separated from the discovery
/// + tokei walk so the threshold + message logic is unit-testable directly.
fn find_crate_violations(
    rule: &CrateSizeRule,
    crate_totals: &[(String, usize)],
) -> Vec<Diagnostic> {
    let lint_id = LintId::CrateSize.id();
    crate_totals
        .iter()
        .filter(|(_, total)| *total > rule.max_code_lines)
        .map(|(rel, total)| {
            at_crate(
                lint_id,
                format!("crate exceeds {} code lines ({total})", rule.max_code_lines),
                rel.clone(),
            )
            .help("split the crate into smaller, more focused crates")
            .note(format!(
                "configured by [[crate-size.rules]] glob = \"{}\"",
                rule.glob.as_str()
            ))
            .build()
        })
        .collect()
}
