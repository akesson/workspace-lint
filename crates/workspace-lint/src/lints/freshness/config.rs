use serde::Deserialize;

#[derive(Deserialize, Clone)]
pub(crate) struct FreshnessConfig {
    pub rules: Vec<FreshnessRule>,
}

#[derive(Deserialize, Clone)]
pub(crate) struct FreshnessRule {
    pub glob: String,
    #[serde(rename = "depends-on")]
    pub depends_on: String,
}
