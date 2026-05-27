//! Layer 3: config-driven entries for macros defined in external crates.
//!
//! Workspace authors declare external-macro behavior in
//! `.workspace-lint.toml`:
//!
//! ```toml
//! [[macros.external]]
//! path = "tokio::main"
//! expansion-uses = ["tokio::runtime::Builder"]
//! ```
//!
//! `syn-workspace` consumes these entries via [`ExternalMacro`] and matches
//! them against macro invocation paths at call sites (after applying Tier 1
//! rename resolution, so `use tokio::main as runtime; #[runtime]` still
//! matches the `tokio::main` entry).

/// One declared external-macro entry from the workspace-lint config.
#[derive(Debug, Clone)]
pub struct ExternalMacro {
    /// Canonical path of the macro definition (e.g. `tokio::main`,
    /// `sqlx::query!`). Trailing `!` is optional and stripped on parse.
    pub path: String,
    /// Paths the macro's expansion references.
    pub expansion_uses: Vec<String>,
}
