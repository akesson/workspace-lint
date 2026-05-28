use fs_err as fs;
use serde::Deserialize;
use std::path::Path;

// Per-lint config structs live next to their lint impls under `crate::lints`.
// `Config` re-exports them so the top-level TOML schema (the user-facing
// `.workspace-lint.toml`) is unchanged.
pub(crate) use crate::lints::architecture::ArchitectureConfig;
pub(crate) use crate::lints::cli_crate_version::CliCrateVersionConfig;
pub(crate) use crate::lints::crate_size::CrateSizeConfig;
pub(crate) use crate::lints::file_size::FileSizeConfig;
pub(crate) use crate::lints::freshness::FreshnessConfig;
pub(crate) use crate::lints::unused_deps::UnusedDepsConfig;
pub(crate) use crate::lints::unused_pub::UnusedPubConfig;

#[derive(Deserialize, Default)]
pub(crate) struct Config {
    #[serde(default)]
    pub checks: Checks,
    /// Per-lint severity overrides. Keys are short kebab names
    /// (`file-size`, `unused-pub`, …); values are `"warn"` or `"deny"`.
    /// Diagnostics whose lint name appears here have their level rewritten
    /// after collection; the process exits with code 1 iff any `Deny`-level
    /// diagnostic survives suppression. Lints absent from this table keep
    /// the default `Warn` level set by each check.
    #[serde(default)]
    pub lints: LintLevels,
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
    #[serde(default)]
    pub architecture: Option<ArchitectureConfig>,
    #[serde(default)]
    pub macros: Option<MacrosConfig>,
}

/// Per-lint severity overrides parsed from the `[lints]` TOML table.
/// Keyed by [`crate::lints::LintId::short`] (kebab form, without the
/// `workspace-lint::` prefix).
#[derive(Deserialize, Default, Debug)]
#[serde(transparent)]
pub(crate) struct LintLevels(pub std::collections::HashMap<String, crate::diagnostic::Level>);

impl LintLevels {
    /// Lookup the configured level for a full lint ID (e.g.
    /// `workspace-lint::file-size`). Returns `None` if not configured.
    pub fn level_for(&self, lint_id: &str) -> Option<crate::diagnostic::Level> {
        let short = lint_id.strip_prefix("workspace-lint::").unwrap_or(lint_id);
        self.0.get(short).copied()
    }
}

#[derive(Deserialize, Default)]
pub(crate) struct Checks {
    #[serde(default, rename = "centralized-deps")]
    pub centralized_deps: bool,
    #[serde(default, rename = "module-tree")]
    pub module_tree: bool,
    #[serde(default, rename = "feature-drift")]
    pub feature_drift: bool,
    #[serde(default)]
    pub visibility: bool,
}

#[derive(Deserialize, Clone)]
pub(crate) struct ExpandConfig {
    pub rules: Vec<ExpandRule>,
}

#[derive(Deserialize, Clone)]
pub(crate) struct ExpandRule {
    pub command: Vec<String>,
    pub glob: String,
    pub marker: String,
    #[serde(default, rename = "auto-stage")]
    pub auto_stage: bool,
}

#[derive(Deserialize, Default, Clone)]
pub(crate) struct MacrosConfig {
    /// External macros (defined outside the workspace) whose expansion
    /// references items the resolver can't see from source alone. Each entry
    /// contributes its `expansion-uses` paths to the workspace-wide
    /// implicit-refs set consulted by visibility / architecture / etc.
    #[serde(default)]
    pub external: Vec<ExternalMacro>,
}

#[derive(Deserialize, Clone)]
pub(crate) struct ExternalMacro {
    /// Canonical path of the external macro, e.g. `tokio::main` or
    /// `sqlx::query`. Currently only used for documentation in the config
    /// — v1 just unions every `expansion-uses` entry into the workspace's
    /// implicit-refs set regardless of which macro it's attached to. A
    /// future version will narrow application to actual invocation sites.
    #[allow(dead_code)]
    pub path: String,
    /// Paths the macro's expansion references. Treated as if these items
    /// were imported at every call site of the macro.
    #[serde(default, rename = "expansion-uses")]
    pub expansion_uses: Vec<String>,
}

const STANDALONE_FILE: &str = ".workspace-lint.toml";

pub(crate) fn load() -> Config {
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

/// Best-effort variant of [`load`]: returns `None` (instead of exiting) if
/// no config file is present. Used by single-check runs that should still
/// honor a project's `[lints]` levels when available but mustn't fail when
/// invoked outside a configured workspace.
pub(crate) fn try_load() -> Option<Config> {
    let standalone_exists = Path::new(STANDALONE_FILE).exists();
    let cargo_metadata = read_cargo_metadata();
    match (standalone_exists, cargo_metadata) {
        (true, None) => {
            let content = fs::read_to_string(STANDALONE_FILE).ok()?;
            toml::from_str(&content).ok()
        }
        (false, Some(raw)) => toml::from_str(&raw).ok(),
        _ => None,
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
    let doc: toml::Value = toml::from_str(cargo_toml_content).ok()?;
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
exclude-crates = ["api", "sdk"]
allowlist = ["*Error", "main"]
kinds = ["function", "struct"]
exclude-paths = ["generated/**"]
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
        assert!(up.exclude_crates.is_empty());
        assert!(up.allowlist.is_empty());
        assert!(up.kinds.is_empty());
        assert!(up.exclude_paths.is_empty());
    }

    #[test]
    fn legacy_on_ci_only_and_schema_are_silently_ignored() {
        // Forward-compat with configs written for the SCIP-era schema. The
        // `on-ci-only` knob (which gated the slow rust-analyzer subprocess)
        // and the `schema` migration version are both gone, but old configs
        // that still carry them must continue to parse — serde drops
        // unknown fields by default and we never opted into
        // `deny_unknown_fields`.
        let toml = r#"
schema = 2

[unused-pub]
on-ci-only = true
"#;
        let config: Config = toml::from_str(toml).expect("legacy fields must not break parse");
        assert!(config.unused_pub.is_some());
    }

    #[test]
    fn parse_unused_pub_full() {
        let toml = r#"
[unused-pub]
exclude-crates = ["api"]
allowlist = ["Error", "*Builder"]
kinds = ["function", "method"]
exclude-paths = ["generated/**", "proto/**"]
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let up = config.unused_pub.unwrap();
        assert_eq!(up.exclude_crates, &["api"]);
        assert_eq!(up.allowlist, &["Error", "*Builder"]);
        assert_eq!(up.kinds, &["function", "method"]);
        assert_eq!(up.exclude_paths, &["generated/**", "proto/**"]);
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
