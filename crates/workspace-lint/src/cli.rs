use clap::{Parser, Subcommand};

use crate::config::{
    CargoFeatures, CliCrateVersionConfig, CliCrateVersionRule, CrateSizeConfig, CrateSizeRule,
    ExpandConfig, ExpandRule, FileSizeConfig, FileSizeRule, FreshnessConfig, FreshnessRule,
    UnusedDepsConfig, UnusedPubConfig,
};

#[derive(Parser)]
#[command(name = "workspace-lint")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
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
pub enum CheckRule {
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
    /// Check for unused public items via SCIP index
    UnusedPub {
        /// Only run this check in CI environments (when CI env var is set)
        #[arg(long, default_value_t = false)]
        on_ci_only: bool,
        /// Path to SCIP index file
        #[arg(long)]
        scip_index: Option<String>,
        /// Crates to exclude from analysis
        #[arg(long)]
        exclude_crates: Vec<String>,
        /// Glob patterns for allowed unused items
        #[arg(long)]
        allowlist: Vec<String>,
        /// Kinds of items to check (e.g. function, struct)
        #[arg(long)]
        kinds: Vec<String>,
        /// Path patterns to exclude
        #[arg(long)]
        exclude_paths: Vec<String>,
        /// Cargo features to enable ("all", "default", or specific features)
        #[arg(long)]
        cargo_features: Vec<String>,
    },
}

impl CheckRule {
    pub fn into_file_size_config(glob: String, max_code_lines: usize) -> FileSizeConfig {
        FileSizeConfig {
            rules: vec![FileSizeRule {
                glob,
                max_code_lines,
            }],
        }
    }

    pub fn into_crate_size_config(
        glob: String,
        max_code_lines: usize,
        include: Vec<String>,
    ) -> CrateSizeConfig {
        CrateSizeConfig {
            rules: vec![CrateSizeRule {
                glob,
                max_code_lines,
                include: if include.is_empty() {
                    None
                } else {
                    Some(include)
                },
            }],
        }
    }

    pub fn into_freshness_config(glob: String, depends_on: String) -> FreshnessConfig {
        FreshnessConfig {
            rules: vec![FreshnessRule { glob, depends_on }],
        }
    }

    pub fn into_cli_crate_version_config(
        command: String,
        pattern: String,
        crate_name: String,
    ) -> CliCrateVersionConfig {
        CliCrateVersionConfig {
            rules: vec![CliCrateVersionRule {
                command: command.split_whitespace().map(String::from).collect(),
                pattern,
                crate_name,
            }],
        }
    }

    pub fn into_unused_deps_config(ignore: Vec<String>) -> UnusedDepsConfig {
        UnusedDepsConfig { ignore }
    }

    pub fn into_unused_pub_config(
        on_ci_only: bool,
        scip_index: Option<String>,
        exclude_crates: Vec<String>,
        allowlist: Vec<String>,
        kinds: Vec<String>,
        exclude_paths: Vec<String>,
        cargo_features: Vec<String>,
    ) -> UnusedPubConfig {
        let cargo_features = if cargo_features.is_empty() {
            CargoFeatures::default()
        } else if cargo_features.len() == 1
            && matches!(cargo_features[0].as_str(), "all" | "default" | "none")
        {
            CargoFeatures::Keyword(cargo_features.into_iter().next().unwrap())
        } else {
            CargoFeatures::List(cargo_features)
        };
        UnusedPubConfig {
            on_ci_only,
            scip_index,
            exclude_crates,
            allowlist,
            kinds,
            exclude_paths,
            cargo_features,
        }
    }

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
