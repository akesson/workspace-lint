// TRUE NEGATIVE (orphan-file) — an `include!` in expression position and an
// `include_str!` of a `.rs` file. The item walk sees neither; rustc opens both.
pub static TABLE: [u8; 4] = include!("gen_table.rs");
pub const SNIPPET: &str = include_str!("gen_snippet.rs");

pub fn first() -> u8 {
    TABLE[0]
}
