use globset::{Glob, GlobSetBuilder};
use std::path::Path;
use syn_workspace::Workspace;
use tokei::{Config as TokeiConfig, Languages};

use crate::diagnostic::Diagnostic;
use crate::diagnostic::builder::at_crate;
use crate::lints::{Lint, LintContext, LintId, Requirements};

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

    fn requirements(&self) -> Requirements {
        Requirements {
            needs_workspace: true,
        }
    }

    fn check(&self, cx: &LintContext<'_>) -> Vec<Diagnostic> {
        let workspace = cx
            .workspace
            .expect("crate-size lint requires Workspace (Requirements::needs_workspace)");
        check(&self.config, workspace)
    }
}

pub(crate) fn check(config: &CrateSizeConfig, workspace: &Workspace) -> Vec<Diagnostic> {
    let lint_id = LintId::CrateSize.id();
    let mut diagnostics = Vec::new();

    for rule in &config.rules {
        let glob = Glob::new(&rule.glob).unwrap_or_else(|e| {
            eprintln!("invalid crate-size glob '{}': {e}", rule.glob);
            std::process::exit(1);
        });
        let matcher = glob.compile_matcher();

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

        // Iterate every workspace member whose workspace-relative manifest
        // directory matches the rule's glob. Replaces the previous
        // `read_dir`-based scan: now the lint can only target real cargo
        // workspace members (cargo's `members`/`exclude`/glob semantics
        // are honored), and the resulting anchor path is workspace-
        // relative for free — `# workspace-lint: allow(crate-size)` in a
        // member Cargo.toml now matches.
        let mut matches: Vec<(String, std::path::PathBuf)> = workspace
            .members()
            .map(|krate| {
                (
                    workspace.crate_relative_path(&krate.manifest_dir),
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

        for (rel, abs) in matches {
            let mut languages = Languages::new();
            // tokei walks the on-disk crate dir for line counting. Pass
            // the absolute path so the walker doesn't depend on the
            // process's working directory.
            let abs_str = abs.display().to_string();
            languages.get_statistics(&[abs_str.as_str()], &[], &TokeiConfig::default());

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
                        rel.clone(),
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
