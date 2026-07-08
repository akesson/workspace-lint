use serde::Deserialize;

use wl_lint_api::config::{GlobPattern, Globs};

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
    /// Minimum distinct *verbatim* identifiers a region must reference — the
    /// names normalization keeps (callees, types, fields, methods; keywords
    /// don't count). Filters "big but empty" boilerplate: a struct literal or
    /// a `HashMap` fill table carries plenty of tokens but almost no distinct
    /// names, and duplicating it is normal. 0 disables.
    #[serde(
        default = "default_min_distinct_anchors",
        rename = "min-distinct-anchors"
    )]
    pub min_distinct_anchors: usize,
    /// Minimum fraction (0.0–1.0) of a region's normalized token stream that
    /// is NOT a repeat of an earlier window in the same region — the classic
    /// RNR filter from token-based clone detection. Suppresses
    /// one-row-stamped-N-times regions (insert tables, assert stutter).
    /// 0.0 disables.
    #[serde(
        default = "default_min_non_repeating_ratio",
        rename = "min-non-repeating-ratio"
    )]
    pub min_non_repeating_ratio: f64,
    /// Maximum independent literal parameters extracting the group may need
    /// (each distinct way the instances' literals co-vary is one parameter of
    /// the unified fn). Groups needing more are data tables — match arms
    /// mapping the same variants to different values — not copy-paste, and
    /// are suppressed. A group carrying a drift violation (a literal breaking
    /// an otherwise consistent mapping) is NEVER suppressed by this gate.
    /// Only meaningful under `ignore-literals`; 0 disables.
    #[serde(default = "default_max_parameters", rename = "max-parameters")]
    pub max_parameters: usize,
    /// Name the refactoring each group calls for — reshape the help line and
    /// append a class note (merge / delete-dead-copy / default-trait-method /
    /// method-on-type / ui-component). Never changes which groups fire; a
    /// group with no named fix keeps the generic help. On by default.
    #[serde(default = "default_true", rename = "classify")]
    pub classify: bool,
    /// Macro names whose duplication reads as UI markup to extract into one
    /// component (Dioxus `rsx!` by default). Only consulted when `classify`.
    #[serde(default = "default_component_macros", rename = "component-macros")]
    pub component_macros: Vec<String>,
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

impl DuplicateCodeConfig {
    /// `check duplicate-code` — one argument per CLI flag. The boolean flags
    /// arrive in opt-in polarity (`--exact-literals`, `--include-test-code`)
    /// and invert onto the config's `ignore-*` fields, so a bare invocation
    /// behaves exactly like a bare `[duplicate-code]` table (pinned by
    /// `tests::bare_cli_matches_bare_table`).
    #[allow(clippy::too_many_arguments)] // one parameter per CLI flag: the flag set is the signature
    pub fn from_cli(
        min_lines: u32,
        min_tokens: usize,
        min_instances: usize,
        exact_literals: bool,
        include_test_code: bool,
        cross_crate_only: bool,
        min_distinct_anchors: usize,
        min_non_repeating_ratio: f64,
        max_parameters: usize,
        no_classify: bool,
        component_macros: &[String],
        include: &[String],
        exclude: &[String],
    ) -> Self {
        Self {
            min_lines,
            min_tokens,
            min_instances,
            ignore_literals: !exact_literals,
            ignore_test_code: !include_test_code,
            cross_crate_only,
            min_distinct_anchors,
            min_non_repeating_ratio,
            max_parameters,
            classify: !no_classify,
            component_macros: if component_macros.is_empty() {
                default_component_macros()
            } else {
                component_macros.to_vec()
            },
            include: Globs(include.iter().map(|p| GlobPattern::from_cli(p)).collect()),
            exclude: Globs(exclude.iter().map(|p| GlobPattern::from_cli(p)).collect()),
        }
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

fn default_min_distinct_anchors() -> usize {
    4
}

fn default_min_non_repeating_ratio() -> f64 {
    0.5
}

fn default_max_parameters() -> usize {
    3
}

fn default_component_macros() -> Vec<String> {
    vec!["rsx".to_string()]
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The flag-polarity inversion in [`DuplicateCodeConfig::from_cli`] must
    /// keep a no-flag `check duplicate-code` identical to the bare table.
    #[test]
    fn bare_cli_matches_bare_table() {
        let table = DuplicateCodeConfig::default();
        let cli = DuplicateCodeConfig::from_cli(
            table.min_lines,
            table.min_tokens,
            table.min_instances,
            false,
            false,
            false,
            table.min_distinct_anchors,
            table.min_non_repeating_ratio,
            table.max_parameters,
            false,
            &[],
            &[],
            &[],
        );
        assert_eq!(cli.ignore_literals, table.ignore_literals);
        assert_eq!(cli.ignore_test_code, table.ignore_test_code);
        assert_eq!(cli.cross_crate_only, table.cross_crate_only);
        assert_eq!(cli.min_distinct_anchors, table.min_distinct_anchors);
        assert_eq!(cli.min_non_repeating_ratio, table.min_non_repeating_ratio);
        assert_eq!(cli.max_parameters, table.max_parameters);
        assert_eq!(cli.classify, table.classify);
        assert_eq!(cli.component_macros, table.component_macros);
        assert!(cli.include.0.is_empty() && cli.exclude.0.is_empty());
    }
}
