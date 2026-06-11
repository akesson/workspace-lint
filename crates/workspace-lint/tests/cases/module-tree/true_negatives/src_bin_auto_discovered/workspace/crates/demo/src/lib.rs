// A `src/bin/*.rs` file is auto-discovered by cargo as a bin target, so it is
// reachable on its own and must NOT be flagged orphan.
pub fn lib_fn() {}
