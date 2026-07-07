use clap::{Parser, Subcommand};

use crate::config::{ExpandConfig, ExpandRule};
use wl_lints::Lint;
use wl_lints::{
    centralized_deps::CentralizedDeps,
    cli_crate_version::CliCrateVersion,
    crate_size::CrateSize,
    duplicate_code::{DuplicateCode, DuplicateCodeConfig},
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
    /// reviewable as one diff. Never deletes code — see `--fix-auto-delete`.
    #[arg(long, global = true, default_value_t = false)]
    pub fix: bool,
    /// Everything `--fix` does, plus: an `unused-pub` item that is unused
    /// everywhere is whole-item DELETED (doc comment through body), cascading
    /// through the entire dead chain in one pass and trimming any `use` left
    /// dangling. Only git-tracked-clean files are touched — the deletion's
    /// backup is `git checkout`. A manual operation by design: there is no
    /// config equivalent, so CI `--fix` runs can never delete code.
    #[arg(long, global = true, default_value_t = false)]
    pub fix_auto_delete: bool,
    /// Skip the clean-git-tree guard used by `--fix` and the `expand`
    /// subcommand, letting them run with uncommitted changes to tracked files.
    /// Off by default so the resulting changes stay reviewable as one diff.
    #[arg(long, global = true, default_value_t = false)]
    pub allow_dirty: bool,
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
    /// Scaffold a default `.workspace-lint.toml` at the workspace root
    Init {
        /// Overwrite an existing `.workspace-lint.toml`
        #[arg(long, default_value_t = false)]
        force: bool,
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
    /// Debug: decode one `.wlir` IR fragment and print it. Reads the rkyv
    /// archive directly (no extraction, no toolchain). `--json` emits serde
    /// JSON (jq-able); otherwise Rust `{:#?}` debug.
    DumpIr {
        /// Path to a `.wlir` fragment file (e.g. `target/workspace-lint/ir/default/foo.wlir`).
        file: std::path::PathBuf,
        /// Emit serde JSON instead of `{:#?}` debug.
        #[arg(long, default_value_t = false)]
        json: bool,
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
    /// Check for structurally identical (Type-2) duplicated code
    DuplicateCode {
        /// Minimum source lines a region must span to be considered
        #[arg(long, default_value_t = DuplicateCodeConfig::default().min_lines)]
        min_lines: u32,
        /// Minimum normalized-token count (guards against dense one-liners)
        #[arg(long, default_value_t = DuplicateCodeConfig::default().min_tokens)]
        min_tokens: usize,
        /// How many structurally identical instances make a finding
        #[arg(long, default_value_t = DuplicateCodeConfig::default().min_instances)]
        min_instances: usize,
        /// Require literal values to match exactly (by default `+ 1` vs `+ 5`
        /// still matches — the copy-paste-and-tweak signature)
        #[arg(long, default_value_t = false)]
        exact_literals: bool,
        /// Also scan test targets, `#[cfg(test)]` items, and `#[test]` fns
        #[arg(long, default_value_t = false)]
        include_test_code: bool,
        /// Report only groups spanning at least two crates
        #[arg(long, default_value_t = false)]
        cross_crate_only: bool,
        /// Minimum distinct verbatim identifiers a region must reference
        /// (noise filter; 0 disables)
        #[arg(long, default_value_t = DuplicateCodeConfig::default().min_distinct_anchors)]
        min_distinct_anchors: usize,
        /// Minimum fraction of the region's token stream not repeating an
        /// earlier window of the same region (noise filter; 0.0 disables)
        #[arg(long, default_value_t = DuplicateCodeConfig::default().min_non_repeating_ratio)]
        min_non_repeating_ratio: f64,
        /// Workspace-relative globs to scan (default: every member source file)
        #[arg(long)]
        include: Vec<String>,
        /// Workspace-relative globs to skip (wins over --include)
        #[arg(long)]
        exclude: Vec<String>,
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
            CheckRule::DuplicateCode {
                min_lines,
                min_tokens,
                min_instances,
                exact_literals,
                include_test_code,
                cross_crate_only,
                min_distinct_anchors,
                min_non_repeating_ratio,
                include,
                exclude,
            } => Box::new(DuplicateCode::new(DuplicateCodeConfig::from_cli(
                min_lines,
                min_tokens,
                min_instances,
                exact_literals,
                include_test_code,
                cross_crate_only,
                min_distinct_anchors,
                min_non_repeating_ratio,
                &include,
                &exclude,
            ))),
            CheckRule::Freshness { glob, depends_on } => {
                Box::new(Freshness::from_cli(glob, depends_on))
            }
            CheckRule::CliCrateVersion {
                command,
                pattern,
                crate_name,
            } => Box::new(CliCrateVersion::from_cli(command, pattern, crate_name)),
            CheckRule::UnusedDeps { ignore } => Box::new(UnusedDeps::from_cli(ignore)),
            CheckRule::UnusedPub { .. } => {
                let config = self
                    .unused_pub_config()
                    .expect("the UnusedPub variant always resolves a config");
                Box::new(UnusedPub::new(config, std::collections::HashMap::new()))
            }
            CheckRule::ModuleTree => Box::new(ModuleTree::new()),
            CheckRule::FeatureDrift => Box::new(FeatureDrift::new()),
            CheckRule::StaleGitIndex => Box::new(StaleGitIndex::new()),
        }
    }

    /// The `UnusedPubConfig` a `check unused-pub` invocation resolves to;
    /// `None` for every other rule. Exposed separately from [`Self::into_lint`]
    /// because the `--fix-auto-delete` cascade needs the same config outside
    /// the boxed `Lint`.
    pub(crate) fn unused_pub_config(&self) -> Option<wl_lints::unused_pub::UnusedPubConfig> {
        match self {
            CheckRule::UnusedPub {
                exclude_crates,
                allowlist,
                kinds,
                exclude_paths,
                suppress_intra_crate,
            } => Some(wl_lints::unused_pub::UnusedPubConfig::from_cli(
                exclude_crates,
                allowlist,
                kinds,
                exclude_paths,
                *suppress_intra_crate,
            )),
            _ => None,
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
                command: wl_lints::util::split_command(&command),
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
    use wl_lints::LintId;

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
    fn into_lint_duplicate_code() {
        let lint = CheckRule::DuplicateCode {
            min_lines: 8,
            min_tokens: 40,
            min_instances: 2,
            exact_literals: false,
            include_test_code: false,
            cross_crate_only: true,
            min_distinct_anchors: 4,
            min_non_repeating_ratio: 0.5,
            include: vec![],
            exclude: vec!["**/generated/**".into()],
        }
        .into_lint();
        assert_eq!(lint.id(), LintId::DuplicateCode);
    }

    /// Every runnable lint must have an ad-hoc `check <short>` subcommand —
    /// the "try it on this repo without touching config" entry point. The
    /// exceptions are enumerated with their reasons; a new lint that lands
    /// without `check` wiring (as `duplicate-code` first did) fails here.
    #[test]
    fn every_runnable_lint_has_a_check_subcommand() {
        use clap::CommandFactory;
        // Not runnable on demand: `config`/`unknown-lint`/`stale-expect` are
        // pipeline findings (config audit / suppression pass), and
        // `architecture` rules are too structured for flags — TOML-only.
        let non_checkable = [
            LintId::Architecture,
            LintId::Config,
            LintId::StaleExpect,
            LintId::UnknownLint,
        ];
        let cmd = Cli::command();
        let check = cmd
            .find_subcommand("check")
            .expect("the check subcommand exists");
        let subcommands: Vec<&str> = check.get_subcommands().map(|c| c.get_name()).collect();
        for id in LintId::ALL {
            if non_checkable.contains(id) {
                continue;
            }
            assert!(
                subcommands.contains(&id.short()),
                "lint `{}` has no `check {}` subcommand (add a CheckRule variant or list it \
                 in non_checkable with a reason)",
                id.short(),
                id.short()
            );
        }
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
