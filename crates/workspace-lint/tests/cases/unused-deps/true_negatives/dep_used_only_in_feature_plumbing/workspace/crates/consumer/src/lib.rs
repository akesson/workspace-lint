//! This crate forwards a Cargo feature to its `provider` dependency
//! (`perf = ["dep:provider"]`) but never names it in code. The dep is genuinely
//! used — enabling `perf` pulls it in — so `unused-deps` must not flag it.

pub fn run() {}
