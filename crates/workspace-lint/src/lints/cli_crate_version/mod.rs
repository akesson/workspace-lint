use fs_err as fs;
use regex::Regex;
use std::process::Command;

use crate::lints::{Lint, LintContext, LintId};
use wl_diagnostic::Diagnostic;
use wl_diagnostic::builder::at_workspace;

pub mod config;
#[cfg(test)]
mod tests;

pub(crate) use config::{CliCrateVersionConfig, CliCrateVersionRule};

pub(crate) struct CliCrateVersion {
    config: CliCrateVersionConfig,
}

impl CliCrateVersion {
    pub(crate) fn new(config: CliCrateVersionConfig) -> Self {
        Self { config }
    }

    pub(crate) fn from_cli(command: String, pattern: String, crate_name: String) -> Self {
        Self::new(CliCrateVersionConfig {
            rules: vec![CliCrateVersionRule {
                command: crate::cli::split_command(&command),
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
    // A missing / unparsable Cargo.lock is a single setup error — surface it
    // as one diagnostic and stop, rather than aborting the whole lint run.
    let lock_packages = match read_lock_packages() {
        Ok(p) => p,
        Err(msg) => {
            return vec![error_diagnostic(
                msg,
                "run `cargo generate-lockfile`, or check that Cargo.lock is valid TOML",
                None,
            )];
        }
    };

    let mut diagnostics = Vec::new();
    for rule in &config.rules {
        match check_rule(rule, &lock_packages) {
            Ok(Some(d)) => diagnostics.push(d),
            Ok(None) => {}
            // A misconfigured or un-runnable rule never kills the run: it
            // becomes its own diagnostic so the other rules (and lints) still
            // report.
            Err(err) => diagnostics.push(error_diagnostic(
                err.message,
                err.help,
                Some(rule.command.join(" ")),
            )),
        }
    }

    diagnostics
}

/// A rule that couldn't be evaluated (bad config or an un-runnable command),
/// carrying the user-facing message and a remediation hint.
#[derive(Debug)]
struct RuleError {
    message: String,
    help: &'static str,
}

/// Evaluate one rule. `Ok(None)` means the CLI matches the lockfile, `Ok(Some)`
/// is a version-mismatch finding, and `Err` is a setup/runtime failure that
/// becomes its own diagnostic (never a process exit).
fn check_rule(
    rule: &CliCrateVersionRule,
    lock_packages: &[(String, String)],
) -> Result<Option<Diagnostic>, RuleError> {
    let (program, args) = rule.command.split_first().ok_or(RuleError {
        message: "cli-crate-version rule has an empty `command`".to_string(),
        help: "set `command` to the CLI invocation, e.g. [\"mytool\", \"--version\"]",
    })?;

    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| RuleError {
            message: format!("failed to run `{}`: {e}", rule.command.join(" ")),
            help: "ensure the tool is installed and on PATH",
        })?;

    if !output.status.success() {
        return Err(RuleError {
            message: format!(
                "`{}` exited unsuccessfully: {}",
                rule.command.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            help: "the version command must exit successfully",
        });
    }

    let re = Regex::new(&rule.pattern).map_err(|e| RuleError {
        message: format!("invalid regex pattern `{}`: {e}", rule.pattern),
        help: "fix the `pattern` regular expression",
    })?;

    let cli_version = extract_version(&output.stdout, &re).ok_or_else(|| RuleError {
        message: format!(
            "pattern `{}` did not match the output of `{}`",
            rule.pattern,
            rule.command.join(" ")
        ),
        help: "the regex must capture the version in group 1",
    })?;

    let lock_version =
        find_lock_version(lock_packages, &rule.crate_name).ok_or_else(|| RuleError {
            message: format!("crate `{}` not found in Cargo.lock", rule.crate_name),
            help: "check the crate name matches a workspace dependency",
        })?;

    if cli_version != lock_version {
        return Ok(Some(
            at_workspace(
                LintId::CliCrateVersion.id(),
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
        ));
    }
    Ok(None)
}

/// Build the diagnostic for a setup/runtime failure. `ran` adds a `ran <cmd>`
/// note when the failure happened while evaluating a specific rule.
fn error_diagnostic(message: String, help: &str, ran: Option<String>) -> Diagnostic {
    let mut builder = at_workspace(LintId::CliCrateVersion.id(), message).help(help.to_string());
    if let Some(cmd) = ran {
        builder = builder.note(format!("ran `{cmd}`"));
    }
    builder.build()
}

/// Extract the version captured by group 1 of `re` from a command's raw stdout,
/// stripping ANSI escapes first and trimming the capture (so a trailing newline
/// or padding doesn't cause a spurious mismatch). `None` if the pattern doesn't
/// match.
fn extract_version(stdout: &[u8], re: &Regex) -> Option<String> {
    let raw = strip_ansi_escapes::strip(stdout);
    let text = String::from_utf8_lossy(&raw);
    re.captures(&text)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().trim().to_string())
}

/// Look up a crate's version in the parsed lockfile package list.
fn find_lock_version<'a>(
    lock_packages: &'a [(String, String)],
    crate_name: &str,
) -> Option<&'a str> {
    lock_packages
        .iter()
        .find(|(name, _)| name == crate_name)
        .map(|(_, version)| version.as_str())
}

fn read_lock_packages() -> Result<Vec<(String, String)>, String> {
    let content =
        fs::read_to_string("Cargo.lock").map_err(|e| format!("failed to read Cargo.lock: {e}"))?;
    parse_lock_packages(&content)
}

fn parse_lock_packages(content: &str) -> Result<Vec<(String, String)>, String> {
    let doc: toml::Value =
        toml::from_str(content).map_err(|e| format!("failed to parse Cargo.lock: {e}"))?;

    Ok(doc
        .get("package")
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
        .unwrap_or_default())
}
