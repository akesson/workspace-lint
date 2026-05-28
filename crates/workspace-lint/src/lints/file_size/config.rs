use serde::Deserialize;

#[derive(Deserialize, Clone)]
pub struct FileSizeConfig {
    pub rules: Vec<FileSizeRule>,
}

#[derive(Deserialize, Clone)]
pub struct FileSizeRule {
    pub glob: String,
    #[serde(rename = "max-code-lines")]
    pub max_code_lines: usize,
}
