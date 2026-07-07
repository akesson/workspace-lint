use serde::Deserialize;

use wl_lint_api::config::{GlobPattern, Globs};

#[derive(Deserialize, Clone)]
pub struct FreshnessConfig {
    pub rules: Vec<FreshnessRule>,
}

#[derive(Deserialize, Clone)]
pub struct FreshnessRule {
    pub glob: GlobPattern,
    /// Dependency globs: a tracked file is stale if any file matching one of
    /// these in its subtree is newer. Accepts a single string or a list.
    #[serde(rename = "depends-on")]
    pub depends_on: Globs,
}
