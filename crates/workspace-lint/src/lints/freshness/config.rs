use serde::Deserialize;

#[derive(Deserialize, Clone)]
pub struct FreshnessConfig {
    pub rules: Vec<FreshnessRule>,
}

#[derive(Deserialize, Clone)]
pub struct FreshnessRule {
    pub glob: String,
    #[serde(rename = "depends-on")]
    pub depends_on: String,
}
