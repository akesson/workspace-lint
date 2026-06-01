use serde::Deserialize;

use crate::config::{GlobPattern, Globs};

#[derive(Deserialize, Clone)]
pub(crate) struct FreshnessConfig {
    pub rules: Vec<FreshnessRule>,
}

#[derive(Deserialize, Clone)]
pub(crate) struct FreshnessRule {
    pub glob: GlobPattern,
    /// Dependency globs: a tracked file is stale if any file matching one of
    /// these in its subtree is newer. Accepts a single string or a list.
    #[serde(rename = "depends-on")]
    pub depends_on: Globs,
}
