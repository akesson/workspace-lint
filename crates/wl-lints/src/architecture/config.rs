use serde::Deserialize;

use wl_lint_api::config::LintLevel;

#[derive(Deserialize, Default, Clone)]
pub struct ArchitectureConfig {
    #[serde(default)]
    pub rules: Vec<ArchitectureRule>,
}

impl ArchitectureConfig {
    /// An `[architecture]` table with no rules is inert (nothing to enforce), so
    /// it counts as "no config table" for enablement. Single source of truth for
    /// this rule, shared by the binary's `Config::has_table_for` and the
    /// `registry` gating so the two can't drift.
    pub fn is_active(&self) -> bool {
        !self.rules.is_empty()
    }
}

#[derive(Deserialize, Clone)]
pub struct ArchitectureRule {
    /// Display name surfaced in diagnostics. Optional but recommended.
    #[serde(default)]
    pub name: Option<String>,
    /// Crate-name globs the rule applies to (the importing crate). Required;
    /// empty means the rule never fires.
    pub from: Vec<String>,
    /// Canonical-path globs of forbidden targets. Required; empty means the
    /// rule never fires.
    pub deny: Vec<String>,
    /// Specific canonical paths in the deny set that are explicitly allowed
    /// (per-rule escape hatch). Matched as globs against canonical paths.
    #[serde(default)]
    pub exceptions: Vec<String>,
    /// Per-rule severity. `None` (the default) means the rule's diagnostics
    /// take the lint's effective level from the `[lints]` table; an explicit
    /// value wins over `[lints] architecture = …` so a deliberate per-rule
    /// severity isn't clobbered by a blanket override. `allow` mutes the rule.
    #[serde(default)]
    pub severity: Option<LintLevel>,
    /// Free-text explanation surfaced in the diagnostic's `note:` line.
    #[serde(default)]
    pub reason: Option<String>,
    /// Suggested alternative surfaced in the diagnostic's `help:` line.
    #[serde(default)]
    pub suggest: Option<String>,
}
