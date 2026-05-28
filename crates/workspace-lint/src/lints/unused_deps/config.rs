use serde::Deserialize;

#[derive(Deserialize, Default, Clone)]
pub struct UnusedDepsConfig {
    #[serde(default)]
    pub ignore: Vec<String>,
}
