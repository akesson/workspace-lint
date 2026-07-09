// TRUE NEGATIVE (orphan-file) — `tests.rs` is named by the source but no
// declared config opens it: the matrix has no `cargo test`. That is a coverage
// gap in the `[engine]` matrix, NOT a dead file, so the lint must not accuse it.
// Contrast `true_negatives/cfg_test_mod_covered_by_matrix`, where adding
// `"cargo test"` silences the gap entirely.
#[cfg(test)]
mod tests;

pub fn go() -> u32 {
    1
}
