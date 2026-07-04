use serde::Deserialize;

#[derive(Deserialize, Clone)]
pub struct CliCrateVersionConfig {
    pub rules: Vec<CliCrateVersionRule>,
}

#[derive(Deserialize, Clone)]
pub struct CliCrateVersionRule {
    pub command: Vec<String>,
    pub pattern: String,
    #[serde(rename = "crate")]
    pub crate_name: String,
}
