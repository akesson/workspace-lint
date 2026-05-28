use serde::Deserialize;

#[derive(Deserialize, Default, Clone)]
pub(crate) struct UnusedDepsConfig {
    #[serde(default)]
    pub ignore: Vec<String>,
}
