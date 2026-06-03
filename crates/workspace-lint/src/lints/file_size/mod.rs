use globset::GlobSetBuilder;
use std::collections::HashMap;
use tokei::{Config as TokeiConfig, Languages};

use crate::config::GlobPattern;
use crate::diagnostic::Diagnostic;
use crate::diagnostic::builder::at_file;
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
    find_violations(&collect_file_lines(), &config.rules)
}

/// Count code lines per file by running tokei from the process cwd. Embedded
/// languages (tokei `children` reports — e.g. Rust inside a doc-code-fence) are
/// summed into the same path key as the host file.
fn collect_file_lines() -> HashMap<String, usize> {
    let mut languages = Languages::new();
    languages.get_statistics(&["."], &[], &TokeiConfig::default());

    let mut file_lines: HashMap<String, usize> = HashMap::new();
    let add = |map: &mut HashMap<String, usize>, name: &std::path::Path, code: usize| {
        let path = name.strip_prefix("./").unwrap_or(name);
        *map.entry(path.display().to_string()).or_default() += code;
    };
    for language in languages.values() {
        for report in &language.reports {
            add(&mut file_lines, &report.name, report.stats.code);
        }
        for child_reports in language.children.values() {
            for report in child_reports {
                add(&mut file_lines, &report.name, report.stats.code);
            }
        }
    }
    file_lines
}

/// Pure projection over per-file code-line counts: emit a diagnostic for every
/// file that exceeds a matching rule's `max_code_lines` (strict `>`). Kept
/// separate from the tokei walk so the glob-match + threshold + message logic
/// is unit-testable without the filesystem. Violations are sorted by path
/// within each rule for deterministic output.
fn find_violations(file_lines: &HashMap<String, usize>, rules: &[FileSizeRule]) -> Vec<Diagnostic> {
    let mut builder = GlobSetBuilder::new();
    for rule in rules {
        builder.add(rule.glob.compiled().clone());
    }
    let globset = builder.build().unwrap();

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
                .help("split #[cfg(test)] modules into separate test files")
                .help("extract related structs, enums, or trait impls into their own modules")
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
