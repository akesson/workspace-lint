use serde::Deserialize;

#[derive(Deserialize, Clone)]
pub struct CrateSizeConfig {
    pub rules: Vec<CrateSizeRule>,
}

#[derive(Deserialize, Clone)]
pub struct CrateSizeRule {
    pub glob: String,
    #[serde(rename = "max-code-lines")]
    pub max_code_lines: usize,
    pub include: Option<Vec<String>>,
}
