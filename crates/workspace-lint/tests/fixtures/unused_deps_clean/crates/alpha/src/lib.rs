use serde::Deserialize;

#[derive(Deserialize)]
pub struct Config {
    pub name: String,
}
