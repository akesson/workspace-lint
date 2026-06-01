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
    let mut builder = GlobSetBuilder::new();
    for rule in &config.rules {
        builder.add(rule.glob.compiled().clone());
    }
    let globset = builder.build().unwrap();

    let mut languages = Languages::new();
    languages.get_statistics(&["."], &[], &TokeiConfig::default());

    let mut file_lines: HashMap<String, usize> = HashMap::new();
    for language in languages.values() {
        for report in &language.reports {
            let path = report.name.strip_prefix("./").unwrap_or(&report.name);
            let key = path.display().to_string();
            *file_lines.entry(key).or_default() += report.stats.code;
        }
        for child_reports in language.children.values() {
            for report in child_reports {
                let path = report.name.strip_prefix("./").unwrap_or(&report.name);
                let key = path.display().to_string();
                *file_lines.entry(key).or_default() += report.stats.code;
            }
        }
    }

    let mut violations: Vec<Vec<(String, usize)>> = vec![Vec::new(); config.rules.len()];

    for (path_str, code_lines) in &file_lines {
        let path = std::path::Path::new(path_str);
        let matches = globset.matches(path);
        for &rule_idx in &matches {
            if *code_lines > config.rules[rule_idx].max_code_lines {
                violations[rule_idx].push((path_str.clone(), *code_lines));
            }
        }
    }

    let lint_id = LintId::FileSize.id();
    let mut diagnostics = Vec::new();
    for (rule_idx, viols) in violations.into_iter().enumerate() {
        let rule = &config.rules[rule_idx];
        for (path, code_lines) in viols {
            diagnostics.push(
                at_file(
                    lint_id,
                    format!(
                        "file exceeds {} code lines ({code_lines})",
                        rule.max_code_lines
                    ),
                    path,
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
