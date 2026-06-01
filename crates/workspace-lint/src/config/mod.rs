use fs_err as fs;
use serde::Deserialize;
use std::path::Path;

mod audit;
mod types;

pub(crate) use types::{GlobPattern, Globs, LintLevel, LintLevels};

use crate::diagnostic::Diagnostic;
use crate::lints::LintId;

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
    /// The one place a lint is enabled and leveled. Keys are short kebab
    /// names (`file-size`, `unused-pub`, …); values are `"allow"`, `"warn"`,
    /// or `"deny"`. The reserved key `default` sets the baseline for every
    /// lint. A lint runs iff its effective level isn't `allow` and — for
    /// *policy* lints ([`LintId::requires_config`]) — its config table is
    /// present. See [`LintLevels::effective`].
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

impl Config {
    /// Whether the config table a *policy* lint ([`LintId::requires_config`])
    /// needs is present. Non-policy lints need no table and always return
    /// `true`. Used by the config audit's "enabled but unconfigured" check.
    pub fn has_table_for(&self, id: LintId) -> bool {
        match id {
            LintId::FileSize => self.file_size.is_some(),
            LintId::CrateSize => self.crate_size.is_some(),
            LintId::Freshness => self.freshness.is_some(),
            LintId::CliCrateVersion => self.cli_crate_version.is_some(),
            LintId::Architecture => self
                .architecture
                .as_ref()
                .is_some_and(|a| !a.rules.is_empty()),
            _ => true,
        }
    }
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

/// Load and parse the project config, returning the typed [`Config`] plus any
/// `config` / `unknown-lint` validation diagnostics (anchored at the config
/// file). Fails fast (process exit) only on genuinely-unusable states: both
/// sources present, no source at all, an unreadable file, unparseable TOML,
/// or a value that can't be interpreted (a bad level string, an uncompilable
/// glob). Everything else is a soft diagnostic the caller merges into the
/// stream — so the linter lints its own config.
pub(crate) fn load() -> (Config, Vec<Diagnostic>) {
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
            let config = parse_config(&content, STANDALONE_FILE);
            let diags = audit::audit(&content, STANDALONE_FILE, &config);
            (config, diags)
        }
        (false, Some(raw)) => {
            let config = parse_config(&raw, "Cargo.toml [workspace.metadata.workspace-lint]");
            // The metadata section is re-serialized (so spans don't map back
            // to Cargo.toml); anchor audit findings at the file itself.
            let diags = audit::audit(&raw, "Cargo.toml", &config);
            (config, diags)
        }
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
    use crate::lints::unused_pub::KindFilter;

    fn parse(toml: &str) -> Config {
        toml::from_str(toml).expect("config should parse")
    }

    fn audit_of(toml: &str) -> Vec<Diagnostic> {
        let config = parse(toml);
        audit::audit(toml, ".workspace-lint.toml", &config)
    }

    fn globs(patterns: &[GlobPattern]) -> Vec<&str> {
        patterns.iter().map(|g| g.as_str()).collect()
    }

    #[test]
    fn parse_full_config() {
        let toml = r#"
[lints]
default = "warn"
centralized-deps = "deny"
unused-pub = "allow"

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

        let config = parse(toml);
        assert_eq!(config.lints.default, Some(LintLevel::Warn));
        assert_eq!(
            config.lints.effective(LintId::CentralizedDeps),
            LintLevel::Deny
        );
        assert_eq!(config.lints.effective(LintId::UnusedPub), LintLevel::Allow);

        let fs_rules = config.file_size.unwrap().rules;
        assert_eq!(fs_rules.len(), 2);
        assert_eq!(fs_rules[0].glob, "**/*.rs");
        assert_eq!(fs_rules[0].max_code_lines, 500);
        assert_eq!(fs_rules[1].glob, "**/*.ts");

        let cs_rules = config.crate_size.unwrap().rules;
        assert_eq!(cs_rules[0].glob, "crates/*");
        assert_eq!(globs(cs_rules[0].include.as_ref().unwrap()), ["*.rs"]);

        let fr_rules = config.freshness.unwrap().rules;
        assert_eq!(fr_rules[0].glob, "**/CLAUDE.md");
        assert_eq!(globs(&fr_rules[0].depends_on.0), ["**/*.rs"]);

        let ex_rules = config.expand.unwrap().rules;
        assert_eq!(ex_rules[0].command, &["mise", "tasks"]);
        assert!(ex_rules[0].auto_stage);

        let cv_rules = config.cli_crate_version.unwrap().rules;
        assert_eq!(cv_rules[0].crate_name, "wasm-bindgen");

        assert_eq!(config.unused_deps.unwrap().ignore, &["prost", "tonic"]);

        let up = config.unused_pub.unwrap();
        assert_eq!(up.exclude_crates, &["api", "sdk"]);
        assert_eq!(globs(&up.allowlist), ["*Error", "main"]);
        assert_eq!(up.kinds, vec![KindFilter::Function, KindFilter::Struct]);
    }

    #[test]
    fn empty_config_warn_default_no_tables() {
        let config = parse("");
        assert!(config.lints.default.is_none());
        // Built-in baseline is warn, so structural lints are on by default.
        assert_eq!(config.lints.effective(LintId::UnusedPub), LintLevel::Warn);
        assert!(config.file_size.is_none());
        assert!(config.unused_pub.is_none());
    }

    #[test]
    fn global_default_allow_disables_structural_but_floors_meta() {
        let config = parse("[lints]\ndefault = \"allow\"\n");
        assert_eq!(config.lints.effective(LintId::UnusedDeps), LintLevel::Allow);
        // Meta-floor: a blanket allow can't silence config validation.
        assert_eq!(config.lints.effective(LintId::Config), LintLevel::Warn);
        assert_eq!(config.lints.effective(LintId::UnknownLint), LintLevel::Warn);
    }

    #[test]
    fn per_lint_override_beats_default() {
        let config = parse("[lints]\ndefault = \"deny\"\nunused-pub = \"allow\"\n");
        assert_eq!(
            config.lints.effective(LintId::CentralizedDeps),
            LintLevel::Deny
        );
        assert_eq!(config.lints.effective(LintId::UnusedPub), LintLevel::Allow);
        assert!(config.lints.has_override(LintId::UnusedPub));
        assert!(!config.lints.has_override(LintId::CentralizedDeps));
    }

    #[test]
    fn unknown_lint_name_is_dropped_from_overrides() {
        // The typed map drops unknown names (the audit reports them); it must
        // not error or masquerade as a real lint.
        let config = parse("[lints]\nunused-dep = \"deny\"\n");
        assert!(config.lints.overrides.is_empty());
    }

    #[test]
    fn unknown_kind_is_a_parse_error() {
        // `method` was documented but never modeled; it now fails fast.
        assert!(toml::from_str::<Config>("[unused-pub]\nkinds = [\"method\"]\n").is_err());
    }

    #[test]
    fn fn_and_mod_kind_aliases_parse() {
        let config = parse("[unused-pub]\nkinds = [\"fn\", \"mod\"]\n");
        let up = config.unused_pub.unwrap();
        assert_eq!(up.kinds, vec![KindFilter::Function, KindFilter::Module]);
    }

    #[test]
    fn parse_unused_pub_defaults() {
        let up = parse("[unused-pub]\n").unused_pub.unwrap();
        assert!(up.allowlist.is_empty());
        assert!(up.kinds.is_empty());
        assert!(up.exclude_paths.is_empty());
    }

    #[test]
    fn parse_crate_size_no_include() {
        let rules = parse("[[crate-size.rules]]\nglob = \"crates/*\"\nmax-code-lines = 5000\n")
            .crate_size
            .unwrap()
            .rules;
        assert!(rules[0].include.is_none());
    }

    #[test]
    fn freshness_depends_on_accepts_string_or_list() {
        let one = parse("[[freshness.rules]]\nglob = \"a\"\ndepends-on = \"**/*.rs\"\n")
            .freshness
            .unwrap()
            .rules;
        assert_eq!(globs(&one[0].depends_on.0), ["**/*.rs"]);

        let many = parse("[[freshness.rules]]\nglob = \"a\"\ndepends-on = [\"x\", \"y\"]\n")
            .freshness
            .unwrap()
            .rules;
        assert_eq!(globs(&many[0].depends_on.0), ["x", "y"]);
    }

    #[test]
    fn architecture_rule_severity_is_optional() {
        let none = parse("[[architecture.rules]]\nfrom = [\"a\"]\ndeny = [\"b\"]\n")
            .architecture
            .unwrap()
            .rules;
        assert_eq!(none[0].severity, None);

        let deny =
            parse("[[architecture.rules]]\nfrom = [\"a\"]\ndeny = [\"b\"]\nseverity = \"deny\"\n")
                .architecture
                .unwrap()
                .rules;
        assert_eq!(deny[0].severity, Some(LintLevel::Deny));
    }

    // --- audit (config validation as diagnostics) ---

    #[test]
    fn audit_clean_config_has_no_findings() {
        let diags = audit_of(
            "[lints]\ncentralized-deps = \"deny\"\n\n[[file-size.rules]]\nglob = \"**/*.rs\"\nmax-code-lines = 500\n",
        );
        assert!(diags.is_empty(), "unexpected: {diags:?}");
    }

    #[test]
    fn audit_flags_unknown_section() {
        let diags = audit_of("[file-siz]\n");
        assert!(
            diags
                .iter()
                .any(|d| d.lint.ends_with("::config") && d.message.contains("file-siz"))
        );
    }

    #[test]
    fn audit_flags_unknown_lint_name() {
        let diags = audit_of("[lints]\nunused-dep = \"deny\"\n");
        let d = diags
            .iter()
            .find(|d| d.lint.ends_with("::unknown-lint"))
            .expect("expected an unknown-lint diagnostic");
        assert!(d.message.contains("unused-dep"));
        assert!(d.helps.iter().any(|h| h.contains("unused-deps")));
    }

    #[test]
    fn audit_flags_unknown_field_in_table() {
        let diags = audit_of("[unused-pub]\nallowlistt = []\n");
        assert!(
            diags
                .iter()
                .any(|d| d.lint.ends_with("::config") && d.message.contains("allowlistt"))
        );
    }

    #[test]
    fn audit_flags_unknown_rule_field() {
        // An *optional* rule field typo is a soft diagnostic; a required-field
        // typo (`max_code_lines`) fails fast as a missing-field parse error.
        let diags = audit_of(
            "[[crate-size.rules]]\nglob = \"a\"\nmax-code-lines = 5\ninclud = [\"*.rs\"]\n",
        );
        assert!(
            diags
                .iter()
                .any(|d| d.lint.ends_with("::config") && d.message.contains("includ"))
        );
    }

    #[test]
    fn audit_flags_policy_lint_enabled_without_config() {
        let diags = audit_of("[lints]\nfile-size = \"deny\"\n");
        assert!(diags.iter().any(|d| {
            d.lint.ends_with("::config")
                && d.message.contains("file-size")
                && d.message.contains("never run")
        }));
    }

    #[test]
    fn audit_does_not_flag_default_unconfigured_policy_lint() {
        // The global `default` leaving policy lints unconfigured is fine; only
        // an *explicit* override without a table is the mistake.
        let diags = audit_of("[lints]\ndefault = \"warn\"\n");
        assert!(diags.is_empty(), "unexpected: {diags:?}");
    }

    // --- extract_metadata_section ---

    #[test]
    fn extract_metadata_with_lints() {
        let cargo_toml = r#"
[workspace]
members = ["crates/*"]

[workspace.metadata.workspace-lint.lints]
centralized-deps = "deny"
"#;
        let raw = extract_metadata_section(cargo_toml).unwrap();
        let config: Config = toml::from_str(&raw).unwrap();
        assert_eq!(
            config.lints.effective(LintId::CentralizedDeps),
            LintLevel::Deny
        );
    }

    #[test]
    fn extract_metadata_with_rules() {
        let cargo_toml = r#"
[workspace]
members = []

[[workspace.metadata.workspace-lint.file-size.rules]]
glob = "**/*.rs"
max-code-lines = 500
"#;
        let raw = extract_metadata_section(cargo_toml).unwrap();
        let rules = toml::from_str::<Config>(&raw)
            .unwrap()
            .file_size
            .unwrap()
            .rules;
        assert_eq!(rules[0].glob, "**/*.rs");
    }

    #[test]
    fn extract_metadata_returns_none_no_workspace() {
        assert!(extract_metadata_section("[package]\nname = \"foo\"\n").is_none());
    }

    #[test]
    fn extract_metadata_returns_none_no_lint_section() {
        let cargo_toml = "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.metadata.other-tool]\nkey = \"value\"\n";
        assert!(extract_metadata_section(cargo_toml).is_none());
    }

    #[test]
    fn parse_expand_defaults() {
        let rules = parse("[[expand.rules]]\ncommand = [\"echo\", \"hi\"]\nglob = \"README.md\"\nmarker = \"HELLO\"\n")
            .expand
            .unwrap()
            .rules;
        assert!(!rules[0].auto_stage);
    }
}
