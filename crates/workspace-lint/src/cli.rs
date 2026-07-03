use clap::{Parser, Subcommand};

use crate::config::{ExpandConfig, ExpandRule};
use crate::lints::Lint;
use crate::lints::{
    centralized_deps::CentralizedDeps,
    cli_crate_version::CliCrateVersion,
    crate_size::CrateSize,
    feature_drift::FeatureDrift,
    file_size::FileSize,
    freshness::Freshness,
    module_tree::ModuleTree,
    stale_git_index::StaleGitIndex,
    unused_deps::UnusedDeps,
    unused_pub::{KindFilter, UnusedPub},
};

#[derive(Parser)]
#[command(name = "workspace-lint")]
pub(crate) struct Cli {
    /// Output format: `human` (default, clippy-style), `json` (rustc-compatible),
    /// or `github` (Actions annotations).
    #[arg(long, global = true)]
    pub message_format: Option<String>,
    /// Apply machine-applicable structural rewrites in-place. Requires a clean
    /// git working tree (override with `--allow-dirty`) so every change is
    /// reviewable as one diff. With deep verification (default; see `--no-deep`)
    /// a directive is auto-written only for a finding rust-analyzer disproves.
    #[arg(long, global = true, default_value_t = false)]
    pub fix: bool,
    /// Skip the clean-git-tree guard used by `--fix` and the `expand`
    /// subcommand, letting them run with uncommitted changes to tracked files.
    /// Off by default so the resulting changes stay reviewable as one diff.
    #[arg(long, global = true, default_value_t = false)]
    pub allow_dirty: bool,
    /// Skip `--fix`'s deep (rust-analyzer SCIP) verification, restoring the
    /// plain "apply machine-applicable fixes" behavior with no second opinion.
    #[arg(long, global = true, default_value_t = false, requires = "fix")]
    pub no_deep: bool,
    /// Use an existing `rust-analyzer scip` index for deep verification instead
    /// of invoking rust-analyzer (for CI caching and hermetic tests).
    #[arg(long, global = true, value_name = "PATH", requires = "fix")]
    pub scip_index: Option<std::path::PathBuf>,
    /// Skip the `cargo check` that harvests build-script env (`OUT_DIR` and
    /// `cargo::rustc-env=` exports) for resolving `include!(concat!(env!(…), …))`
    /// generated code. Keeps the run fully offline and subprocess-light; literal
    /// and `CARGO_*`-based includes still resolve. By default the harvest runs
    /// only for crates that have both a `build.rs` and an `include!`, so a
    /// workspace without that combination already pays nothing.
    #[arg(long, global = true, default_value_t = false)]
    pub no_build_env: bool,
    /// Run only the build-free lints — skip the rustc-backed semantic tier
    /// (and its pinned-toolchain requirement) entirely.
    #[arg(long, global = true, default_value_t = false)]
    pub fast_only: bool,
    /// Debug: run the extraction+assembly tier and print its stats, then exit
    /// without running any lints. Requires the pinned toolchain.
    #[arg(long, global = true, hide = true, default_value_t = false)]
    pub engine_dump: bool,
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
        /// Crates to exclude from analysis
        #[arg(long)]
        exclude_crates: Vec<String>,
        /// Glob patterns for allowed unused items (matched against canonical paths)
        #[arg(long)]
        allowlist: Vec<String>,
        /// Kinds of items to check (e.g. function, struct, trait)
        #[arg(long, value_enum)]
        kinds: Vec<KindFilter>,
        /// Path patterns to exclude (matched against source file paths)
        #[arg(long)]
        exclude_paths: Vec<String>,
        /// Suppress the "only used inside the crate" variant — only report
        /// items with zero references anywhere.
        #[arg(long, default_value_t = false)]
        suppress_intra_crate: bool,
    },
    /// Check module-tree structural integrity (broken `mod`s, orphan files)
    ModuleTree,
    /// Check for feature drift (declared-but-unused / undeclared features)
    FeatureDrift,
    /// Check for paths tracked by git that no longer exist on disk
    StaleGitIndex,
}

impl CheckRule {
    /// Map a single `check <rule>` subcommand invocation to the concrete
    /// `Lint` it exercises. Every lint construction lives in its own
    /// `from_cli` constructor inside `lints/<name>/`; this method is the
    /// thin dispatch table that wires the CheckRule variants to them.
    pub(crate) fn into_lint(self) -> Box<dyn Lint> {
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
                exclude_crates,
                allowlist,
                kinds,
                exclude_paths,
                suppress_intra_crate,
            } => Box::new(UnusedPub::from_cli(
                exclude_crates,
                allowlist,
                kinds,
                exclude_paths,
                suppress_intra_crate,
            )),
            CheckRule::ModuleTree => Box::new(ModuleTree::new()),
            CheckRule::FeatureDrift => Box::new(FeatureDrift::new()),
            CheckRule::StaleGitIndex => Box::new(StaleGitIndex::new()),
        }
    }

    /// Build an `ExpandConfig` from the `expand` subcommand's CLI args.
    /// `expand` is not a lint (it side-effects), so it keeps its own helper.
    pub(crate) fn into_expand_config(
        command: String,
        glob: String,
        marker: String,
        auto_stage: bool,
    ) -> ExpandConfig {
        ExpandConfig {
            rules: vec![ExpandRule {
                command: split_command(&command),
                glob,
                marker,
                auto_stage,
            }],
        }
    }
}

/// Split a CLI `--command` string into argv using shell-like quoting, so
/// `--command "tool --flag 'a b'"` survives args with spaces (the old naive
/// whitespace split mangled them). Exits with a clear message on unbalanced
/// quotes.
pub(crate) fn split_command(command: &str) -> Vec<String> {
    shell_words::split(command).unwrap_or_else(|e| {
        eprintln!("error: could not parse --command `{command}`: {e}");
        std::process::exit(2);
    })
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
