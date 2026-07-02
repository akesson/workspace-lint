//! Stub `strum`: carries the one item the (stubbed) `EnumString` expansion
//! references, mirroring the real derive's `impl FromStr { type Err =
//! strum::ParseError; … }`. The fixture never names `strum` in SOURCE — the
//! reference lives only in generated code, which is exactly what the case
//! exercises on both backends (syn: the name-keyed assertion; rustc: the
//! post-expansion HIR edge).
#[derive(Debug)]
pub enum ParseError {
    VariantNotFound,
}
