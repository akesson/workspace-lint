use fs_err as fs;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

mod audit;
mod types;

pub(crate) use types::{GlobPattern, Globs, LintLevel, LintLevels};

use crate::lints::LintId;
use wl_diagnostic::Diagnostic;
use wl_engine::fast::FastModel;

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
    /// Per-crate config tier, keyed by Cargo package name (`[crates.<name>]`).
    /// Each entry can carry per-crate lint *levels* (`[crates.<name>.lints]`,
    /// for any lint) and per-crate *params* for the two lints whose params are
    /// workspace-flat and have no glob escape hatch — `unused-deps` /
    /// `unused-pub`. See [`CrateConfig`] and [`Config::effective_level`].
    #[serde(default)]
    pub crates: HashMap<String, CrateConfig>,
    /// The `[engine]` table: how the rustc-backed semantic tier extracts.
    /// Absent table ⇒ the default single-config matrix. See [`EngineSection`].
    #[serde(default)]
    pub engine: EngineSection,
}

/// The `[engine]` table: the cargo-configuration matrix the full (rustc-backed)
/// tier extracts under. Only consulted when a semantic lint is enabled (or the
/// hidden `--engine-dump` runs) — a plain run never touches the engine.
#[derive(Deserialize, Clone)]
pub(crate) struct EngineSection {
    /// Config selectors, first entry = primary (it defines the candidate set
    /// downstream). Accepted entries: `"default"` (plain `cargo check`) and
    /// `"--tests"` (alias `"tests"`). Unknown entries draw a `config`
    /// diagnostic from the audit and are skipped by [`Self::selectors`].
    #[serde(default = "default_engine_configs")]
    pub configs: Vec<String>,
}

impl Default for EngineSection {
    fn default() -> Self {
        Self {
            configs: default_engine_configs(),
        }
    }
}

fn default_engine_configs() -> Vec<String> {
    vec!["default".into()]
}

impl EngineSection {
    /// Every accepted `configs` entry spelling (`tests` aliases `--tests`).
    /// The audit's "did you mean …?" candidates; kept next to
    /// [`Self::selector_for`] so the two can't drift.
    pub(crate) const KNOWN: &'static [&'static str] =
        &["default", "--tests", "tests", "--benches", "benches"];

    /// The engine selector one entry maps to; `None` for an unknown entry
    /// (already reported by the config audit).
    pub(crate) fn selector_for(entry: &str) -> Option<wl_engine::CfgSelector> {
        match entry {
            "default" => Some(wl_engine::CfgSelector::default_cfg()),
            "--tests" | "tests" => Some(wl_engine::CfgSelector::tests()),
            "--benches" | "benches" => Some(wl_engine::CfgSelector::benches()),
            _ => None,
        }
    }

    /// The extraction matrix, in declaration order (first = primary). Unknown
    /// entries are skipped (the audit already flagged them). An empty result —
    /// an explicit `configs = []` or all entries unknown — falls back to the
    /// default config so extraction always has a primary.
    pub(crate) fn selectors(&self) -> Vec<wl_engine::CfgSelector> {
        let mut out: Vec<wl_engine::CfgSelector> = self
            .configs
            .iter()
            .filter_map(|e| Self::selector_for(e))
            .collect();
        if out.is_empty() {
            out.push(wl_engine::CfgSelector::default_cfg());
        }
        out
    }
}

/// One `[crates.<name>]` block: the per-crate middle tier of the lint cascade.
/// Levels in `lints` override the global `[lints]` for this crate only; a
/// present `unused-deps` / `unused-pub` section *wholesale-replaces* the global
/// one for this crate (predictable: "this crate's config is exactly what's
/// written"). Param sections for any other lint are rejected by the audit —
/// glob-based lints (`file-size`, …) already scope per-crate via their globs.
#[derive(Deserialize, Default, Clone)]
pub(crate) struct CrateConfig {
    #[serde(default)]
    pub lints: LintLevels,
    #[serde(default, rename = "unused-deps")]
    pub unused_deps: Option<UnusedDepsConfig>,
    #[serde(default, rename = "unused-pub")]
    pub unused_pub: Option<UnusedPubConfig>,
}

impl Config {
    /// Whether the config table a *policy* lint ([`LintId::requires_config`])
    /// needs is present. Non-policy lints need no table and always return
    /// `true`. Used by the config audit's "enabled but unconfigured" check.
    pub(crate) fn has_table_for(&self, id: LintId) -> bool {
        match id {
            LintId::FileSize => self.file_size.is_some(),
            LintId::CrateSize => self.crate_size.is_some(),
            LintId::Freshness => self.freshness.is_some(),
            LintId::CliCrateVersion => self.cli_crate_version.is_some(),
            LintId::Architecture => self
                .architecture
                .as_ref()
                .is_some_and(ArchitectureConfig::is_active),
            _ => true,
        }
    }

    /// Effective level for `id` in the context of crate `krate` (the Cargo
    /// package name, or `None` for a workspace/config-level diagnostic that
    /// belongs to no single crate). Cascade, narrowest wins:
    ///
    /// 1. per-crate per-lint override (`[crates.<krate>.lints] <id> = …`)
    /// 2. per-crate baseline (`[crates.<krate>.lints] default = …`)
    /// 3. global per-lint override / baseline / built-in `Warn`
    ///    (delegated to [`LintLevels::effective`])
    ///
    /// The `config` / `unknown-lint` meta-floor applies at every *baseline*
    /// step, so neither a global nor a per-crate `default = "allow"` can hide a
    /// broken config — only an explicit per-lint entry can.
    pub(crate) fn effective_level(&self, id: LintId, krate: Option<&str>) -> LintLevel {
        if let Some(cc) = krate.and_then(|name| self.crates.get(name)) {
            if let Some(level) = cc.lints.overrides.get(&id) {
                return *level;
            }
            if let Some(base) = cc.lints.default {
                return LintLevels::floor_meta(id, base);
            }
        }
        self.lints.effective(id)
    }

    /// The `unused-deps` params for crate `name`: its per-crate section if
    /// present, else the global `[unused-deps]`, else defaults. Used by the
    /// registry to capture per-crate params in the lint instance.
    pub(crate) fn unused_deps_overrides(&self) -> HashMap<String, UnusedDepsConfig> {
        self.crates
            .iter()
            .filter_map(|(name, cc)| cc.unused_deps.clone().map(|c| (name.clone(), c)))
            .collect()
    }

    /// The `unused-pub` params for each crate that declares a per-crate section
    /// (see [`Config::unused_deps_overrides`]).
    pub(crate) fn unused_pub_overrides(&self) -> HashMap<String, UnusedPubConfig> {
        self.crates
            .iter()
            .filter_map(|(name, cc)| cc.unused_pub.clone().map(|c| (name.clone(), c)))
            .collect()
    }
}

/// Validate the per-crate config tier's crate *names* against the resolved
/// workspace membership — a `[crates.<name>]` block whose `<name>` isn't a
/// member is almost always a typo or a stale entry, and (because the config is
/// centralized) would otherwise silently do nothing. Emits a `config`
/// diagnostic with a "did you mean …?" hint. Kept separate from the pure-TOML
/// [`audit::audit`] because it needs the loaded [`FastModel`].
pub(crate) fn audit_crate_membership(config: &Config, fast: &FastModel) -> Vec<Diagnostic> {
    if config.crates.is_empty() {
        return Vec::new();
    }
    let members: Vec<String> = fast.members().iter().map(|c| c.name.clone()).collect();
    audit::audit_crate_names(config, &members, &config_source_path())
}

/// Re-derive which config source is in effect (standalone file vs. Cargo.toml
/// metadata) for anchoring diagnostics emitted outside [`load`]. Mirrors the
/// dispatch in [`load`]; both can't be present (that's a fail-fast there).
fn config_source_path() -> String {
    if Path::new(STANDALONE_FILE).exists() {
        STANDALONE_FILE.to_string()
    } else {
        "Cargo.toml".to_string()
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

pub(crate) const STANDALONE_FILE: &str = ".workspace-lint.toml";

/// Whether a standalone `.workspace-lint.toml` is present in the cwd. Used by
/// the `init` scaffolder to refuse clobbering an existing config.
pub(crate) fn standalone_config_exists() -> bool {
    Path::new(STANDALONE_FILE).exists()
}

/// Whether config already lives in `Cargo.toml`'s
/// `[workspace.metadata.workspace-lint]`. Used by `init` so it never scaffolds
/// a second, conflicting config source (loading both is a hard error — see
/// [`load`]).
pub(crate) fn metadata_config_exists() -> bool {
    read_cargo_metadata().is_some()
}

/// Parse `toml_str` as a [`Config`] and run the raw-key audit against it,
/// returning any `config` / `unknown-lint` findings. Test-only: the `init`
/// scaffolder uses it to prove its emitted template is self-clean, so a future
/// renamed/removed section can't make the default config self-flag.
#[cfg(test)]
pub(crate) fn audit_str(toml_str: &str) -> Vec<Diagnostic> {
    let config: Config = toml::from_str(toml_str).expect("template config parses");
    audit::audit(toml_str, STANDALONE_FILE, &config)
}

/// Load and parse the project config, returning the typed [`Config`] plus any
/// `config` / `unknown-lint` validation diagnostics (anchored at the config
/// file). Fails fast (process exit) only on genuinely-unusable states: both
/// sources present, no source at all, an unreadable file, unparsable TOML,
/// or a value that can't be interpreted (a bad level string, an uncompilable
/// glob). Everything else is a soft diagnostic the caller merges into the
/// stream — so the linter lints its own config.
pub(crate) fn load() -> (Config, Vec<Diagnostic>) {
    let standalone_exists = Path::new(STANDALONE_FILE).exists();
    let cargo_metadata = read_cargo_metadata();

    match (standalone_exists, cargo_metadata) {
        (true, Some(_)) => crate::util::fail(format!(
            "error: found both {STANDALONE_FILE} and [workspace.metadata.workspace-lint] in Cargo.toml — use only one"
        )),
        (false, None) => crate::util::fail(format!(
            "error: no configuration found — run `workspace-lint init` to scaffold {STANDALONE_FILE}, or add [workspace.metadata.workspace-lint] to Cargo.toml"
        )),
        (true, None) => {
            let content = fs::read_to_string(STANDALONE_FILE).unwrap_or_else(|e| {
                crate::util::fail(format!("failed to read {STANDALONE_FILE}: {e}"))
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
    toml::from_str(toml_str)
        .unwrap_or_else(|e| crate::util::fail(format!("failed to parse config from {source}: {e}")))
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
mod tests;
