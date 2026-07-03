use globset::{Glob, GlobSetBuilder};
use std::path::{Path, PathBuf};
use tokei::{Config as TokeiConfig, Languages};
use wl_engine::fast::FastModel;

use crate::config::GlobPattern;
use crate::diagnostic::Diagnostic;
use crate::diagnostic::builder::at_crate;
use crate::lints::{Lint, LintContext, LintId, Requirements};

pub mod config;
#[cfg(test)]
mod tests;

use crate::lints::shipped_source;

pub(crate) use config::{CrateSizeConfig, CrateSizeRule};

pub(crate) struct CrateSize {
    config: CrateSizeConfig,
}

impl CrateSize {
    pub(crate) fn new(config: CrateSizeConfig) -> Self {
        Self { config }
    }

    pub(crate) fn from_cli(glob: String, max_code_lines: usize, include: Vec<String>) -> Self {
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

impl Lint for CrateSize {
    fn id(&self) -> LintId {
        LintId::CrateSize
    }

    fn requirements(&self) -> Requirements {
        Requirements {
            needs_fast: true,
            ..Requirements::default()
        }
    }

    fn check(&self, cx: &LintContext<'_>) -> Vec<Diagnostic> {
        let fast = cx
            .fast
            .expect("crate-size lint requires FastModel (Requirements::needs_fast)");
        check(&self.config, fast)
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
        let mut include_builder = GlobSetBuilder::new();
        match &rule.include {
            Some(patterns) => {
                for p in patterns {
                    include_builder.add(p.compiled().clone());
                }
            }
            None => {
                include_builder.add(Glob::new("*.rs").expect("`*.rs` is a valid glob"));
            }
        }
        let include_set = include_builder.build().unwrap();

        // Iterate every workspace member whose workspace-relative manifest
        // directory matches the rule's glob. Replaces the previous
        // `read_dir`-based scan: now the lint can only target real cargo
        // workspace members (cargo's `members`/`exclude`/glob semantics
        // are honored), and the resulting anchor path is workspace-
        // relative for free — `# workspace-lint: allow(crate-size)` in a
        // member Cargo.toml now matches.
        let mut matches: Vec<(String, std::path::PathBuf)> = fast
            .members()
            .iter()
            .map(|krate| {
                (
                    fast.crate_relative_path(&krate.manifest_dir),
                    krate.manifest_dir.clone(),
                )
            })
            .filter_map(|(rel, abs)| {
                let key = rel.display().to_string();
                if matcher.is_match(&key) {
                    Some((key, abs))
                } else {
                    None
                }
            })
            .collect();
        matches.sort_by(|a, b| a.0.cmp(&b.0));

        let crate_totals: Vec<(String, usize)> = matches
            .into_iter()
            .map(|(rel, abs)| (rel, count_crate_code(&abs, &include_set)))
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
