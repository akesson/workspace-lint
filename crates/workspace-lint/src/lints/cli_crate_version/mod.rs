use fs_err as fs;
use regex::Regex;
use std::process::Command;

use crate::diagnostic::Diagnostic;
use crate::diagnostic::builder::at_workspace;
use crate::lints::{Lint, LintContext, LintId};

pub mod config;
#[cfg(test)]
mod tests;

pub(crate) use config::{CliCrateVersionConfig, CliCrateVersionRule};

pub(crate) struct CliCrateVersion {
    config: CliCrateVersionConfig,
}

impl CliCrateVersion {
    pub fn new(config: CliCrateVersionConfig) -> Self {
        Self { config }
    }

    pub fn from_cli(command: String, pattern: String, crate_name: String) -> Self {
        Self::new(CliCrateVersionConfig {
            rules: vec![CliCrateVersionRule {
                command: command.split_whitespace().map(String::from).collect(),
                pattern,
                crate_name,
            }],
        })
    }
}

impl Lint for CliCrateVersion {
    fn id(&self) -> LintId {
        LintId::CliCrateVersion
    }

    fn check(&self, _cx: &LintContext<'_>) -> Vec<Diagnostic> {
        check(&self.config)
    }
}

pub(crate) fn check(config: &CliCrateVersionConfig) -> Vec<Diagnostic> {
    let lint_id = LintId::CliCrateVersion.id();
    let lock_packages = read_lock_packages();
    let mut diagnostics = Vec::new();

    for rule in &config.rules {
        let (program, args) = rule.command.split_first().unwrap_or_else(|| {
            eprintln!("cli-crate-version: command must not be empty");
            std::process::exit(1);
        });

        let output = Command::new(program)
            .args(args)
            .output()
            .unwrap_or_else(|e| {
                eprintln!(
                    "cli-crate-version: failed to run `{}`: {e}",
                    rule.command.join(" ")
                );
                std::process::exit(1);
            });

        if !output.status.success() {
            eprintln!(
                "cli-crate-version: `{}` failed: {}",
                rule.command.join(" "),
                String::from_utf8_lossy(&output.stderr)
            );
            std::process::exit(1);
        }

        let raw = strip_ansi_escapes::strip(&output.stdout);
        let stdout = String::from_utf8_lossy(&raw);

        let re = Regex::new(&rule.pattern).unwrap_or_else(|e| {
            eprintln!(
                "cli-crate-version: invalid regex pattern `{}`: {e}",
                rule.pattern
            );
            std::process::exit(1);
        });

        let cli_version = re
            .captures(&stdout)
            .and_then(|caps| caps.get(1))
            .map(|m| m.as_str())
            .unwrap_or_else(|| {
                eprintln!(
                    "cli-crate-version: pattern `{}` did not match output of `{}`",
                    rule.pattern,
                    rule.command.join(" ")
                );
                std::process::exit(1);
            });

        let lock_version = lock_packages
            .iter()
            .find(|(name, _)| name == &rule.crate_name)
            .map(|(_, version)| version.as_str())
            .unwrap_or_else(|| {
                eprintln!(
                    "cli-crate-version: crate `{}` not found in Cargo.lock",
                    rule.crate_name
                );
                std::process::exit(1);
            });

        if cli_version != lock_version {
            diagnostics.push(
                at_workspace(
                    lint_id,
                    format!(
                        "`{}` CLI version {cli_version} does not match Cargo.lock {lock_version}",
                        rule.crate_name
                    ),
                )
                .help(format!(
                    "update or reinstall `{}` to match the workspace version",
                    rule.crate_name
                ))
                .note(format!("ran `{}`", rule.command.join(" ")))
                .build(),
            );
        }
    }

    diagnostics
}

fn read_lock_packages() -> Vec<(String, String)> {
    let content = fs::read_to_string("Cargo.lock").unwrap_or_else(|e| {
        eprintln!("failed to read Cargo.lock: {e}");
        std::process::exit(1);
    });
    parse_lock_packages(&content)
}

fn parse_lock_packages(content: &str) -> Vec<(String, String)> {
    let doc: toml::Value = toml::from_str(content).unwrap_or_else(|e| {
        eprintln!("failed to parse Cargo.lock: {e}");
        std::process::exit(1);
    });

    doc.get("package")
        .and_then(|p| p.as_array())
        .map(|packages| {
            packages
                .iter()
                .filter_map(|pkg| {
                    let name = pkg.get("name")?.as_str()?;
                    let version = pkg.get("version")?.as_str()?;
                    Some((name.to_string(), version.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}
