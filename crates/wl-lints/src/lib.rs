//! Every lint implementation, one module per lint.
//!
//! Each check lives in `crates/wl-lints/src/<name>/` and implements
//! [`wl_lint_api::Lint`]. The vocabulary the lints build on — the trait,
//! `LintContext`, `LintId`, the config primitives, and the `git`/`util`/
//! `surgery` helpers — is the `wl-lint-api` crate; the *registry* that binds
//! enabled lints to a loaded `Config` is the binary's composition root
//! (`crates/workspace-lint/src/registry.rs`).
//!
//! ## Adding a new lint
//!
//! 1. Create `crates/wl-lints/src/<name>/{mod.rs,config.rs,tests.rs}`.
//! 2. Add a `LintId` variant in `wl-lint-api`'s `lints_id.rs`.
//! 3. Add a line in the binary's `registry::registry` wiring the lint up to its
//!    config block.
//! 4. Add a scenario in the binary's `messages::scenarios()` (asserted by the
//!    registry-coverage test).
//! 5. Add fixtures under `tests/cases/<name>/` (and `tests/fixtures/fix__<name>/`
//!    if the lint emits machine-applicable structural fixes).

pub mod architecture;
pub mod centralized_deps;
pub mod cli_crate_version;
pub mod crate_size;
pub mod duplicate_code;
pub mod feature_drift;
pub mod file_size;
pub mod orphan_file;
pub mod stale_git_index;
pub mod unused_deps;
pub mod unused_pub;

/// Production-only line counting shared by `crate-size` and `file-size`
/// (lives in `wl-fast` with the other syntactic scanners; imported here so
/// lint modules keep their `crate::shipped_source` paths).
use wl_engine::fast::shipped_source;
