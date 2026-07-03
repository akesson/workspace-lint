//! Just enough of serde_with's classic API for the fixture's
//! `#[serde(with = "::serde_with::rust::display_fromstr")]` field to compile.
pub mod rust {
    pub mod display_fromstr {
        pub fn serialize<T, S>(value: &T, serializer: S) -> Result<S::Ok, S::Error>
        where
            T: core::fmt::Display,
            S: serde::Serializer,
        {
            serializer.collect_str(value)
        }
    }
}
