use serde::Serialize;

// `serde_with` is named only inside the `with = "…"` string literal — invisible
// to every scan but the serde-with assertion, which parses the absolute path.
#[derive(Serialize)]
pub struct Config {
    #[serde(with = "::serde_with::rust::display_fromstr")]
    pub level: u32,
}
