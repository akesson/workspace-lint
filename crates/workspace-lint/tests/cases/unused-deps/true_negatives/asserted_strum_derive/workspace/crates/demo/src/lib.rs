use strum_macros::EnumString;

// `strum_macros` is referenced by the `use` above; `strum` is referenced only
// through the code `EnumString` expands to — visible only to the strum-derive
// assertion. Without it, `strum` is flagged as an unused dependency.
#[derive(EnumString)]
pub enum Color {
    Red,
    Green,
}
