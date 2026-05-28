use globset::{Glob, GlobSetBuilder};
use std::path::Path;
use tokei::{Config as TokeiConfig, Languages};

use crate::diagnostic::Diagnostic;
use crate::diagnostic::builder::at_crate;
use crate::lints::{Lint, LintContext, LintId};

pub mod config;
#[cfg(test)]
mod tests;

pub(crate) use config::{CrateSizeConfig, CrateSizeRule};

pub(crate) struct CrateSize {
    config: CrateSizeConfig,
}

impl CrateSize {
    pub fn new(config: CrateSizeConfig) -> Self {
        Self { config }
    }

    pub fn from_cli(glob: String, max_code_lines: usize, include: Vec<String>) -> Self {
        Self::new(CrateSizeConfig {
            rules: vec![CrateSizeRule {
                glob,
                max_code_lines,
                include: if include.is_empty() {
                    None
                } else {
                    Some(include)
                },
            }],
        })
    }
}

impl Lint for CrateSize {
    fn id(&self) -> LintId {
        LintId::CrateSize
    }

    fn check(&self, _cx: &LintContext<'_>) -> Vec<Diagnostic> {
        check(&self.config)
    }
}

pub(crate) fn check(config: &CrateSizeConfig) -> Vec<Diagnostic> {
    let lint_id = LintId::CrateSize.id();
    let mut diagnostics = Vec::new();

    for rule in &config.rules {
        let dirs = expand_glob(&rule.glob);
        let include_set = rule.include.as_ref().map(|patterns| {
            let mut builder = GlobSetBuilder::new();
            for p in patterns {
                builder.add(Glob::new(p).unwrap_or_else(|e| {
                    eprintln!("invalid include pattern '{p}': {e}");
                    std::process::exit(1);
                }));
            }
            builder.build().unwrap()
        });

        for dir in &dirs {
            let mut languages = Languages::new();
            languages.get_statistics(&[dir.as_str()], &[], &TokeiConfig::default());

            let mut total_code: usize = 0;
            for language in languages.values() {
                for report in &language.reports {
                    if let Some(ref gs) = include_set {
                        let name = report.name.file_name().unwrap_or_default();
                        if !gs.is_match(Path::new(name)) {
                            continue;
                        }
                    }
                    total_code += report.stats.code;
                }
            }

            if total_code > rule.max_code_lines {
                diagnostics.push(
                    at_crate(
                        lint_id,
                        format!(
                            "crate exceeds {} code lines ({total_code})",
                            rule.max_code_lines
                        ),
                        dir.clone(),
                    )
                    .help("split the crate into smaller, more focused crates")
                    .note(format!(
                        "configured by [[crate-size.rules]] glob = \"{}\"",
                        rule.glob
                    ))
                    .build(),
                );
            }
        }
    }

    diagnostics
}

fn expand_glob(pattern: &str) -> Vec<String> {
    let glob = Glob::new(pattern).unwrap_or_else(|e| {
        eprintln!("invalid crate-size glob '{pattern}': {e}");
        std::process::exit(1);
    });
    let matcher = glob.compile_matcher();

    let parent = pattern
        .find(['*', '?', '['])
        .map(|pos| &pattern[..pattern[..pos].rfind('/').map(|i| i + 1).unwrap_or(0)])
        .unwrap_or(pattern);

    let parent_path = if parent.is_empty() {
        Path::new(".")
    } else {
        Path::new(parent)
    };

    let mut dirs = Vec::new();
    if let Ok(entries) = std::fs::read_dir(parent_path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let rel = path
                    .strip_prefix("./")
                    .unwrap_or(&path)
                    .display()
                    .to_string();
                if matcher.is_match(&rel) {
                    dirs.push(rel);
                }
            }
        }
    }

    dirs.sort();
    dirs
}
