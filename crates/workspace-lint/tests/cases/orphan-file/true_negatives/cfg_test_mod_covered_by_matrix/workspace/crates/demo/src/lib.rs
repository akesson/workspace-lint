// TRUE NEGATIVE (orphan-file) — `tests.rs` is cfg-stripped before file loading
// under `cargo build`, so only the `cargo test` config opens it. The union over
// the matrix is what makes it live.
#[cfg(test)]
mod tests;

pub fn go() -> u32 {
    1
}
