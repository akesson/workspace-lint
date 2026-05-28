//! Layer 3: caller-supplied entries for macros defined in external crates.
//!
//! Where Layers 1 and 2 discover references by inspecting `macro_rules!`
//! bodies in the workspace itself, this layer accepts declarative entries
//! from outside the resolver — typically read from a config file or
//! hardcoded by the consumer. A consumer might declare entries like:
//!
//! ```toml
//! # In whatever config file the consumer chooses to use.
//! [[macros.external]]
//! path = "tokio::main"
//! expansion-uses = ["tokio::runtime::Builder"]
//! ```
//!
//! and then call [`crate::Workspace::register_external_macro_uses`] to
//! feed the parsed paths into the workspace model. Matching against
//! invocation sites happens after Tier 1 rename resolution, so
//! `use tokio::main as runtime; #[runtime]` matches the `tokio::main`
//! entry.

/// One declared external-macro entry — typically constructed by a
/// consumer from its own config format.
#[derive(Debug, Clone)]
pub struct ExternalMacro {
    /// Canonical path of the macro definition (e.g. `tokio::main`,
    /// `sqlx::query!`). Trailing `!` is optional and stripped on parse.
    pub path: String,
    /// Paths the macro's expansion references.
    pub expansion_uses: Vec<String>,
}
