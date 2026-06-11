use serde::Serialize;

// `Config` is the crate's published public API, so it's exempt. The `with`
// helpers below are pub (serde requires it) but sit in a *private* module, so
// they are not externally reachable — without the serde-with assertion crediting
// `date_fmt::{serialize, deserialize}`, they'd read as "appears unused".
#[derive(Serialize)]
pub struct Config {
    #[serde(with = "date_fmt")]
    pub when: u64,
}

mod date_fmt {
    use serde::Serializer;

    pub fn serialize<S>(value: &u64, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u64(*value)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<u64, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        serde::Deserialize::deserialize(deserializer)
    }
}
