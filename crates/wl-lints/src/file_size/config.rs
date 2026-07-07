use serde::Deserialize;

use wl_lint_api::config::GlobPattern;

#[derive(Deserialize, Clone)]
pub struct FileSizeConfig {
    pub rules: Vec<FileSizeRule>,
}

#[derive(Deserialize, Clone)]
pub struct FileSizeRule {
    pub glob: GlobPattern,
    #[serde(rename = "max-code-lines")]
    pub max_code_lines: usize,
}
