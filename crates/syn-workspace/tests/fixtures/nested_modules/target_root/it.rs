// Simulates an integration-test target root (e.g. `tests/it.rs`): a crate root
// whose filename stem is not lib/main/mod. Its `mod common;` must resolve to the
// sibling `common/mod.rs`, NOT `it/common/mod.rs`. Reached only by the
// `target_root_resolves_sibling_submodule` unit test, never by `build_crate_tree`.
pub mod common;
