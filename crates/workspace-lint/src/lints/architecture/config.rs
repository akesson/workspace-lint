use serde::Deserialize;

#[derive(Deserialize, Default, Clone)]
pub(crate) struct ArchitectureConfig {
    #[serde(default)]
    pub rules: Vec<ArchitectureRule>,
}

#[derive(Deserialize, Clone)]
pub(crate) struct ArchitectureRule {
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
    #[serde(default)]
    pub severity: ArchSeverity,
    /// Free-text explanation surfaced in the diagnostic's `note:` line.
    #[serde(default)]
    pub reason: Option<String>,
    /// Suggested alternative surfaced in the diagnostic's `help:` line.
    #[serde(default)]
    pub suggest: Option<String>,
}

#[derive(Deserialize, Debug, Default, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ArchSeverity {
    #[default]
    Warn,
    Deny,
}
