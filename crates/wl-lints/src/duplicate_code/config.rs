use serde::Deserialize;

use crate::config::Globs;

/// `[duplicate-code]` — a *policy* lint: the table's presence is the opt-in
/// (every field has a sensible default, so an empty table is a valid,
/// active configuration — unlike `architecture`, there is no "inert" shape).
#[derive(Deserialize, Clone)]
pub struct DuplicateCodeConfig {
    /// Minimum source lines a region must span to be considered.
    #[serde(default = "default_min_lines", rename = "min-lines")]
    pub min_lines: u32,
    /// Minimum normalized-token count — the guard against dense one-liners
    /// (a match arm list, a builder chain) that clear the line bar.
    #[serde(default = "default_min_tokens", rename = "min-tokens")]
    pub min_tokens: usize,
    /// How many structurally identical instances make a finding.
    #[serde(default = "default_min_instances", rename = "min-instances")]
    pub min_instances: usize,
    /// Treat differing literal values (`+ 1` vs `+ 5`, differing strings) as
    /// still-matching. On by default: literal-only differences are the
    /// classic copy-paste-and-tweak signature.
    #[serde(default = "default_true", rename = "ignore-literals")]
    pub ignore_literals: bool,
    /// Skip test code: `tests/`/`benches/`/`examples/` targets entirely,
    /// plus `#[cfg(test)]` items and `#[test]`-marked fns in shipped files.
    /// Test boilerplate is legitimately repetitive.
    #[serde(default = "default_true", rename = "ignore-test-code")]
    pub ignore_test_code: bool,
    /// Report only groups spanning at least two crates — the high-value
    /// "helper copy-pasted between crates" case.
    #[serde(default, rename = "cross-crate-only")]
    pub cross_crate_only: bool,
    /// Workspace-relative globs to scan. Empty (the default) means every
    /// member source file.
    #[serde(default)]
    pub include: Globs,
    /// Workspace-relative globs to skip (wins over `include`).
    #[serde(default)]
    pub exclude: Globs,
}

impl Default for DuplicateCodeConfig {
    fn default() -> Self {
        // Field-by-field duplication of the serde defaults would drift;
        // deserializing the empty table *is* the default definition.
        toml::from_str("").expect("empty table satisfies all field defaults")
    }
}

fn default_min_lines() -> u32 {
    8
}

fn default_min_tokens() -> usize {
    40
}

fn default_min_instances() -> usize {
    2
}

fn default_true() -> bool {
    true
}
