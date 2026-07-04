use serde::Deserialize;

use crate::config::GlobPattern;

#[derive(Deserialize, Clone)]
pub struct CrateSizeConfig {
    pub rules: Vec<CrateSizeRule>,
}

#[derive(Deserialize, Clone)]
pub struct CrateSizeRule {
    pub glob: GlobPattern,
    #[serde(rename = "max-code-lines")]
    pub max_code_lines: usize,
    /// File-name globs selecting which files count toward the budget.
    /// Defaults to `["*.rs"]` — Rust source only, so committed data (JSON
    /// oracle snapshots, TOML fixtures) under a crate dir doesn't inflate it.
    /// Set explicitly to count other file types or to narrow the set.
    pub include: Option<Vec<GlobPattern>>,
}
