use clap::{Parser, Subcommand};

use crate::config::{ExpandConfig, ExpandRule};
use crate::lints::Lint;
use crate::lints::{
    centralized_deps::CentralizedDeps, cli_crate_version::CliCrateVersion, crate_size::CrateSize,
    file_size::FileSize, freshness::Freshness, unused_deps::UnusedDeps, unused_pub::UnusedPub,
};

#[derive(Parser)]
#[command(name = "workspace-lint")]
pub(crate) struct Cli {
    /// Output format: `human` (default, clippy-style), `json` (rustc-compatible),
    /// or `github` (Actions annotations).
    #[arg(long, global = true)]
    pub message_format: Option<String>,
    /// Apply machine-applicable structural rewrites in-place. Lints without a
    /// structural fix are reported but left untouched; `--fix` never inserts
    /// silence directives on your behalf.
    #[arg(long, global = true, default_value_t = false)]
    pub fix: bool,
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub(crate) enum Commands {
    /// Run a single lint check
    Check {
        #[command(subcommand)]
        rule: CheckRule,
    },
    /// Mark freshness targets as up-to-date (requires TOML config)
    Done,
    /// Expand markers in files with command output
    Expand {
        /// Command to run (e.g. "mise tasks")
        #[arg(long)]
        command: String,
        /// Glob pattern for files to expand
        #[arg(long)]
        glob: String,
        /// Marker name to replace
        #[arg(long)]
        marker: String,
        /// Auto-stage modified files in git
        #[arg(long, default_value_t = false)]
        auto_stage: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum CheckRule {
    /// Check that workspace dependencies are centralized
    CentralizedDeps,
    /// Check file sizes against limits
    FileSize {
        /// Glob pattern for files to check
        #[arg(long)]
        glob: String,
        /// Maximum number of code lines
        #[arg(long)]
        max_code_lines: usize,
    },
    /// Check crate sizes against limits
    CrateSize {
        /// Glob pattern for crates to check
        #[arg(long)]
        glob: String,
        /// Maximum number of code lines
        #[arg(long)]
        max_code_lines: usize,
        /// File patterns to include in counting
        #[arg(long)]
        include: Vec<String>,
    },
    /// Check that files are fresher than their dependencies
    Freshness {
        /// Glob pattern for files to check
        #[arg(long)]
        glob: String,
        /// Glob pattern for dependency files
        #[arg(long)]
        depends_on: String,
    },
    /// Check that a CLI tool version matches the crate version
    CliCrateVersion {
        /// Command to run (e.g. "wasm-bindgen --version")
        #[arg(long)]
        command: String,
        /// Regex pattern to extract version from command output
        #[arg(long)]
        pattern: String,
        /// Crate name to compare against
        #[arg(long, rename_all = "kebab-case")]
        crate_name: String,
    },
    /// Check for unused dependencies
    UnusedDeps {
        /// Dependencies to ignore
        #[arg(long)]
        ignore: Vec<String>,
    },
    /// Check for unused public items via the resolver-backed cross-crate index
    UnusedPub {
        /// Only run this check in CI environments (when CI env var is set)
        #[arg(long, default_value_t = false)]
        on_ci_only: bool,
        /// Crates to exclude from analysis
        #[arg(long)]
        exclude_crates: Vec<String>,
        /// Glob patterns for allowed unused items (matched against canonical paths)
        #[arg(long)]
        allowlist: Vec<String>,
        /// Kinds of items to check (e.g. fn, struct, trait)
        #[arg(long)]
        kinds: Vec<String>,
        /// Path patterns to exclude (matched against source file paths)
        #[arg(long)]
        exclude_paths: Vec<String>,
        /// Suppress the "only used inside the crate" variant — only report
        /// items with zero references anywhere.
        #[arg(long, default_value_t = false)]
        suppress_intra_crate: bool,
    },
}

impl CheckRule {
    /// Map a single `check <rule>` subcommand invocation to the concrete
    /// `Lint` it exercises. Every lint construction lives in its own
    /// `from_cli` constructor inside `lints/<name>/`; this method is the
    /// thin dispatch table that wires the CheckRule variants to them.
    pub fn into_lint(self) -> Box<dyn Lint> {
        match self {
            CheckRule::CentralizedDeps => Box::new(CentralizedDeps::new()),
            CheckRule::FileSize {
                glob,
                max_code_lines,
            } => Box::new(FileSize::from_cli(glob, max_code_lines)),
            CheckRule::CrateSize {
                glob,
                max_code_lines,
                include,
            } => Box::new(CrateSize::from_cli(glob, max_code_lines, include)),
            CheckRule::Freshness { glob, depends_on } => {
                Box::new(Freshness::from_cli(glob, depends_on))
            }
            CheckRule::CliCrateVersion {
                command,
                pattern,
                crate_name,
            } => Box::new(CliCrateVersion::from_cli(command, pattern, crate_name)),
            CheckRule::UnusedDeps { ignore } => Box::new(UnusedDeps::from_cli(ignore)),
            CheckRule::UnusedPub {
                on_ci_only,
                exclude_crates,
                allowlist,
                kinds,
                exclude_paths,
                suppress_intra_crate,
            } => Box::new(UnusedPub::from_cli(
                on_ci_only,
                exclude_crates,
                allowlist,
                kinds,
                exclude_paths,
                suppress_intra_crate,
            )),
        }
    }

    /// Build an `ExpandConfig` from the `expand` subcommand's CLI args.
    /// `expand` is not a lint (it side-effects), so it keeps its own helper.
    pub fn into_expand_config(
        command: String,
        glob: String,
        marker: String,
        auto_stage: bool,
    ) -> ExpandConfig {
        ExpandConfig {
            rules: vec![ExpandRule {
                command: command.split_whitespace().map(String::from).collect(),
                glob,
                marker,
                auto_stage,
            }],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lints::LintId;

    #[test]
    fn into_lint_centralized_deps() {
        let lint = CheckRule::CentralizedDeps.into_lint();
        assert_eq!(lint.id(), LintId::CentralizedDeps);
    }

    #[test]
    fn into_lint_file_size() {
        let lint = CheckRule::FileSize {
            glob: "**/*.rs".into(),
            max_code_lines: 500,
        }
        .into_lint();
        assert_eq!(lint.id(), LintId::FileSize);
    }

    #[test]
    fn into_lint_crate_size() {
        let lint = CheckRule::CrateSize {
            glob: "crates/*".into(),
            max_code_lines: 5000,
            include: vec!["*.rs".into()],
        }
        .into_lint();
        assert_eq!(lint.id(), LintId::CrateSize);
    }

    #[test]
    fn into_lint_freshness() {
        let lint = CheckRule::Freshness {
            glob: "**/CLAUDE.md".into(),
            depends_on: "**/*.rs".into(),
        }
        .into_lint();
        assert_eq!(lint.id(), LintId::Freshness);
    }

    #[test]
    fn into_lint_cli_crate_version() {
        let lint = CheckRule::CliCrateVersion {
            command: "wasm-bindgen --version".into(),
            pattern: r"wasm-bindgen (\S+)".into(),
            crate_name: "wasm-bindgen".into(),
        }
        .into_lint();
        assert_eq!(lint.id(), LintId::CliCrateVersion);
    }

    #[test]
    fn into_lint_unused_deps() {
        let lint = CheckRule::UnusedDeps {
            ignore: vec!["serde".into()],
        }
        .into_lint();
        assert_eq!(lint.id(), LintId::UnusedDeps);
    }

    #[test]
    fn into_lint_unused_pub() {
        let lint = CheckRule::UnusedPub {
            on_ci_only: false,
            exclude_crates: vec![],
            allowlist: vec![],
            kinds: vec![],
            exclude_paths: vec![],
            suppress_intra_crate: false,
        }
        .into_lint();
        assert_eq!(lint.id(), LintId::UnusedPub);
    }

    #[test]
    fn into_expand_config_splits_command_whitespace() {
        let cfg = CheckRule::into_expand_config(
            "mise tasks".into(),
            "CLAUDE.md".into(),
            "MISE_TASKS".into(),
            true,
        );
        assert_eq!(cfg.rules.len(), 1);
        assert_eq!(cfg.rules[0].command, vec!["mise", "tasks"]);
        assert_eq!(cfg.rules[0].glob, "CLAUDE.md");
        assert_eq!(cfg.rules[0].marker, "MISE_TASKS");
        assert!(cfg.rules[0].auto_stage);
    }
}
