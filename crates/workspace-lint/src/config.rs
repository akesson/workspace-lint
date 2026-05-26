use fs_err as fs;
use serde::Deserialize;
use std::path::Path;

#[derive(Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub checks: Checks,
    #[serde(default, rename = "file-size")]
    pub file_size: Option<FileSizeConfig>,
    #[serde(default, rename = "crate-size")]
    pub crate_size: Option<CrateSizeConfig>,
    #[serde(default)]
    pub freshness: Option<FreshnessConfig>,
    #[serde(default)]
    pub expand: Option<ExpandConfig>,
    #[serde(default, rename = "cli-crate-version")]
    pub cli_crate_version: Option<CliCrateVersionConfig>,
    #[serde(default, rename = "unused-deps")]
    pub unused_deps: Option<UnusedDepsConfig>,
    #[serde(default, rename = "unused-pub")]
    pub unused_pub: Option<UnusedPubConfig>,
}

#[derive(Deserialize, Default)]
pub struct Checks {
    #[serde(default, rename = "centralized-deps")]
    pub centralized_deps: bool,
}

#[derive(Deserialize)]
pub struct ExpandConfig {
    pub rules: Vec<ExpandRule>,
}

#[derive(Deserialize)]
pub struct ExpandRule {
    pub command: Vec<String>,
    pub glob: String,
    pub marker: String,
    #[serde(default, rename = "auto-stage")]
    pub auto_stage: bool,
}

#[derive(Deserialize)]
pub struct CliCrateVersionConfig {
    pub rules: Vec<CliCrateVersionRule>,
}

#[derive(Deserialize)]
pub struct CliCrateVersionRule {
    pub command: Vec<String>,
    pub pattern: String,
    #[serde(rename = "crate")]
    pub crate_name: String,
}

#[derive(Deserialize, Default)]
pub struct UnusedDepsConfig {
    #[serde(default)]
    pub ignore: Vec<String>,
}

#[derive(Deserialize, Default)]
pub struct UnusedPubConfig {
    #[serde(default, rename = "on-ci-only")]
    pub on_ci_only: bool,
    #[serde(default, rename = "scip-index")]
    pub scip_index: Option<String>,
    #[serde(default, rename = "exclude-crates")]
    pub exclude_crates: Vec<String>,
    #[serde(default)]
    pub allowlist: Vec<String>,
    #[serde(default)]
    pub kinds: Vec<String>,
    #[serde(default, rename = "exclude-paths")]
    pub exclude_paths: Vec<String>,
    #[serde(default = "CargoFeatures::default_all", rename = "cargo-features")]
    pub cargo_features: CargoFeatures,
}

#[derive(Deserialize, Debug, PartialEq)]
#[serde(untagged)]
pub enum CargoFeatures {
    Keyword(String),
    List(Vec<String>),
}

impl Default for CargoFeatures {
    fn default() -> Self {
        Self::default_all()
    }
}

impl CargoFeatures {
    fn default_all() -> Self {
        CargoFeatures::Keyword("all".to_string())
    }
}

#[derive(Deserialize)]
pub struct FileSizeConfig {
    pub rules: Vec<FileSizeRule>,
}

#[derive(Deserialize)]
pub struct FileSizeRule {
    pub glob: String,
    #[serde(rename = "max-code-lines")]
    pub max_code_lines: usize,
}

#[derive(Deserialize)]
pub struct CrateSizeConfig {
    pub rules: Vec<CrateSizeRule>,
}

#[derive(Deserialize)]
pub struct CrateSizeRule {
    pub glob: String,
    #[serde(rename = "max-code-lines")]
    pub max_code_lines: usize,
    pub include: Option<Vec<String>>,
}

#[derive(Deserialize)]
pub struct FreshnessConfig {
    pub rules: Vec<FreshnessRule>,
}

#[derive(Deserialize)]
pub struct FreshnessRule {
    pub glob: String,
    #[serde(rename = "depends-on")]
    pub depends_on: String,
}

const STANDALONE_FILE: &str = ".workspace-lint.toml";

pub fn load() -> Config {
    let standalone_exists = Path::new(STANDALONE_FILE).exists();
    let cargo_metadata = read_cargo_metadata();

    match (standalone_exists, cargo_metadata) {
        (true, Some(_)) => {
            eprintln!(
                "error: found both {STANDALONE_FILE} and [workspace.metadata.workspace-lint] in Cargo.toml — use only one"
            );
            std::process::exit(1);
        }
        (false, None) => {
            eprintln!(
                "error: no configuration found — create {STANDALONE_FILE} or add [workspace.metadata.workspace-lint] to Cargo.toml"
            );
            std::process::exit(1);
        }
        (true, None) => {
            let content = fs::read_to_string(STANDALONE_FILE).unwrap_or_else(|e| {
                eprintln!("failed to read {STANDALONE_FILE}: {e}");
                std::process::exit(1);
            });
            parse_config(&content, STANDALONE_FILE)
        }
        (false, Some(raw)) => parse_config(&raw, "Cargo.toml [workspace.metadata.workspace-lint]"),
    }
}

fn parse_config(toml_str: &str, source: &str) -> Config {
    toml::from_str(toml_str).unwrap_or_else(|e| {
        eprintln!("failed to parse config from {source}: {e}");
        std::process::exit(1);
    })
}

/// Extract the `[workspace.metadata.workspace-lint]` section from raw Cargo.toml content,
/// re-serialized as a standalone TOML string so we can deserialize it into Config.
fn extract_metadata_section(cargo_toml_content: &str) -> Option<String> {
    let doc: toml::Value = cargo_toml_content.parse().ok()?;
    let section = doc
        .get("workspace")?
        .get("metadata")?
        .get("workspace-lint")?;
    Some(toml::to_string(section).expect("failed to re-serialize workspace-lint metadata"))
}

/// Read the `[workspace.metadata.workspace-lint]` section from Cargo.toml.
fn read_cargo_metadata() -> Option<String> {
    let content = fs::read_to_string("Cargo.toml").ok()?;
    extract_metadata_section(&content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_config() {
        let toml = r#"
[checks]
centralized-deps = true

[[file-size.rules]]
glob = "**/*.rs"
max-code-lines = 500

[[file-size.rules]]
glob = "**/*.ts"
max-code-lines = 300

[[crate-size.rules]]
glob = "crates/*"
max-code-lines = 5000
include = ["*.rs"]

[[crate-size.rules]]
glob = "crates/web-*"
max-code-lines = 8000
include = ["*.rs", "*.ts"]

[[freshness.rules]]
glob = "**/CLAUDE.md"
depends-on = "**/*.rs"

[[expand.rules]]
command = ["mise", "tasks"]
glob = "CLAUDE.md"
marker = "MISE_TASKS"
auto-stage = true

[[cli-crate-version.rules]]
command = ["wasm-bindgen", "--version"]
pattern = "wasm-bindgen (\\S+)"
crate = "wasm-bindgen"

[unused-deps]
ignore = ["prost", "tonic"]

[unused-pub]
scip-index = "index.scip"
exclude-crates = ["api", "sdk"]
allowlist = ["*Error", "main"]
kinds = ["function", "struct"]
exclude-paths = ["generated/**"]
cargo-features = "all"
"#;

        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.checks.centralized_deps);

        let fs_rules = config.file_size.unwrap().rules;
        assert_eq!(fs_rules.len(), 2);
        assert_eq!(fs_rules[0].glob, "**/*.rs");
        assert_eq!(fs_rules[0].max_code_lines, 500);
        assert_eq!(fs_rules[1].glob, "**/*.ts");
        assert_eq!(fs_rules[1].max_code_lines, 300);

        let cs_rules = config.crate_size.unwrap().rules;
        assert_eq!(cs_rules.len(), 2);
        assert_eq!(cs_rules[0].glob, "crates/*");
        assert_eq!(cs_rules[0].max_code_lines, 5000);
        assert_eq!(cs_rules[0].include.as_ref().unwrap(), &["*.rs"]);
        assert_eq!(cs_rules[1].include.as_ref().unwrap(), &["*.rs", "*.ts"]);

        let fr_rules = config.freshness.unwrap().rules;
        assert_eq!(fr_rules.len(), 1);
        assert_eq!(fr_rules[0].glob, "**/CLAUDE.md");
        assert_eq!(fr_rules[0].depends_on, "**/*.rs");

        let ex_rules = config.expand.unwrap().rules;
        assert_eq!(ex_rules.len(), 1);
        assert_eq!(ex_rules[0].command, &["mise", "tasks"]);
        assert_eq!(ex_rules[0].glob, "CLAUDE.md");
        assert_eq!(ex_rules[0].marker, "MISE_TASKS");
        assert!(ex_rules[0].auto_stage);

        let cv_rules = config.cli_crate_version.unwrap().rules;
        assert_eq!(cv_rules.len(), 1);
        assert_eq!(cv_rules[0].command, &["wasm-bindgen", "--version"]);
        assert_eq!(cv_rules[0].pattern, "wasm-bindgen (\\S+)");
        assert_eq!(cv_rules[0].crate_name, "wasm-bindgen");

        let ud = config.unused_deps.unwrap();
        assert_eq!(ud.ignore, &["prost", "tonic"]);

        let up = config.unused_pub.unwrap();
        assert_eq!(up.scip_index.as_deref(), Some("index.scip"));
        assert_eq!(up.exclude_crates, &["api", "sdk"]);
        assert_eq!(up.allowlist, &["*Error", "main"]);
        assert_eq!(up.kinds, &["function", "struct"]);
    }

    #[test]
    fn parse_empty_config_defaults_all_disabled() {
        let config: Config = toml::from_str("").unwrap();
        assert!(!config.checks.centralized_deps);
        assert!(config.file_size.is_none());
        assert!(config.crate_size.is_none());
        assert!(config.freshness.is_none());
        assert!(config.expand.is_none());
        assert!(config.cli_crate_version.is_none());
        assert!(config.unused_deps.is_none());
        assert!(config.unused_pub.is_none());
    }

    #[test]
    fn parse_partial_checks() {
        let toml = r#"
[checks]
centralized-deps = true
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.checks.centralized_deps);
    }

    #[test]
    fn parse_only_file_size_rules() {
        let toml = r#"
[[file-size.rules]]
glob = "**/*.rs"
max-code-lines = 400
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let rules = config.file_size.unwrap().rules;
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].max_code_lines, 400);
    }

    #[test]
    fn parse_unused_deps_defaults() {
        let toml = r#"
[unused-deps]
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let ud = config.unused_deps.unwrap();
        assert!(ud.ignore.is_empty());
    }

    #[test]
    fn parse_unused_pub_defaults() {
        let toml = r#"
[unused-pub]
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let up = config.unused_pub.unwrap();
        assert!(!up.on_ci_only);
        assert!(up.scip_index.is_none());
        assert!(up.exclude_crates.is_empty());
        assert!(up.allowlist.is_empty());
        assert!(up.kinds.is_empty());
        assert!(up.exclude_paths.is_empty());
        assert_eq!(up.cargo_features, CargoFeatures::Keyword("all".to_string()));
    }

    #[test]
    fn parse_unused_pub_on_ci_only() {
        let toml = r#"
[unused-pub]
on-ci-only = true
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let up = config.unused_pub.unwrap();
        assert!(up.on_ci_only);
    }

    #[test]
    fn parse_unused_pub_full() {
        let toml = r#"
[unused-pub]
scip-index = "target/index.scip"
exclude-crates = ["api"]
allowlist = ["Error", "*Builder"]
kinds = ["function", "method"]
exclude-paths = ["generated/**", "proto/**"]
cargo-features = "default"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let up = config.unused_pub.unwrap();
        assert_eq!(up.scip_index.as_deref(), Some("target/index.scip"));
        assert_eq!(up.exclude_crates, &["api"]);
        assert_eq!(up.allowlist, &["Error", "*Builder"]);
        assert_eq!(up.kinds, &["function", "method"]);
        assert_eq!(up.exclude_paths, &["generated/**", "proto/**"]);
        assert_eq!(
            up.cargo_features,
            CargoFeatures::Keyword("default".to_string())
        );
    }

    #[test]
    fn parse_unused_pub_cargo_features_list() {
        let toml = r#"
[unused-pub]
cargo-features = ["feat1", "feat2"]
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let up = config.unused_pub.unwrap();
        assert_eq!(
            up.cargo_features,
            CargoFeatures::List(vec!["feat1".to_string(), "feat2".to_string()])
        );
    }

    #[test]
    fn parse_crate_size_no_include() {
        let toml = r#"
[[crate-size.rules]]
glob = "crates/*"
max-code-lines = 5000
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let rules = config.crate_size.unwrap().rules;
        assert!(rules[0].include.is_none());
    }

    #[test]
    fn parse_multiple_freshness_rules() {
        let toml = r#"
[[freshness.rules]]
glob = "**/CLAUDE.md"
depends-on = "**/*.rs"

[[freshness.rules]]
glob = "**/README.md"
depends-on = "**/*.ts"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let rules = config.freshness.unwrap().rules;
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[1].glob, "**/README.md");
    }

    #[test]
    fn parse_unknown_keys_are_ignored() {
        let toml = r#"
[checks]
centralized-deps = true
unknown-future-check = true
"#;
        // serde default ignores unknown keys (no deny_unknown_fields)
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.checks.centralized_deps);
    }

    #[test]
    fn parse_cli_crate_version_multiple_rules() {
        let toml = r#"
[[cli-crate-version.rules]]
command = ["tool-a", "--version"]
pattern = "(\\S+)"
crate = "tool-a"

[[cli-crate-version.rules]]
command = ["tool-b", "--version"]
pattern = "v(\\S+)"
crate = "tool-b"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let rules = config.cli_crate_version.unwrap().rules;
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[1].crate_name, "tool-b");
    }

    // --- extract_metadata_section ---

    #[test]
    fn extract_metadata_with_checks() {
        let cargo_toml = r#"
[workspace]
members = ["crates/*"]

[workspace.metadata.workspace-lint]
[workspace.metadata.workspace-lint.checks]
centralized-deps = true
"#;
        let raw = extract_metadata_section(cargo_toml).unwrap();
        let config: Config = toml::from_str(&raw).unwrap();
        assert!(config.checks.centralized_deps);
    }

    #[test]
    fn extract_metadata_with_rules() {
        let cargo_toml = r#"
[workspace]
members = []

[workspace.metadata.workspace-lint]

[[workspace.metadata.workspace-lint.file-size.rules]]
glob = "**/*.rs"
max-code-lines = 500
"#;
        let raw = extract_metadata_section(cargo_toml).unwrap();
        let config: Config = toml::from_str(&raw).unwrap();
        let rules = config.file_size.unwrap().rules;
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].glob, "**/*.rs");
        assert_eq!(rules[0].max_code_lines, 500);
    }

    #[test]
    fn extract_metadata_returns_none_no_workspace() {
        let cargo_toml = r#"
[package]
name = "foo"
version = "0.1.0"
"#;
        assert!(extract_metadata_section(cargo_toml).is_none());
    }

    #[test]
    fn extract_metadata_returns_none_no_metadata() {
        let cargo_toml = r#"
[workspace]
members = ["crates/*"]
"#;
        assert!(extract_metadata_section(cargo_toml).is_none());
    }

    #[test]
    fn extract_metadata_returns_none_no_lint_section() {
        let cargo_toml = r#"
[workspace]
members = ["crates/*"]

[workspace.metadata.other-tool]
key = "value"
"#;
        assert!(extract_metadata_section(cargo_toml).is_none());
    }

    #[test]
    fn parse_expand_defaults() {
        let toml = r#"
[[expand.rules]]
command = ["echo", "hello"]
glob = "README.md"
marker = "HELLO"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let rules = config.expand.unwrap().rules;
        assert_eq!(rules.len(), 1);
        assert!(!rules[0].auto_stage);
    }
}
