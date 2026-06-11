// lib.rs doesn't declare a `mod orphan;` — yet orphan.rs exists in src/.
// The module-tree lint should flag the file as unreachable.
pub fn public_fn() {}
