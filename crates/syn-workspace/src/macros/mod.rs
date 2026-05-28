//! Macro reference tracking — the three layers that feed downstream lints
//! information about what items a macro's expansion references.
//!
//! - [`autodetect`] (Layer 1): scans `macro_rules!` bodies and proc-macro
//!   source for token-level identifiers.
//! - [`annotation`] (Layer 2): parses `expansion_uses!(...)` invocations and
//!   `// workspace-syn: expansion-uses(...)` comment directives that appear
//!   immediately before a macro definition.
//! - [`external`] (Layer 3): reads `[[macros.external]]` entries from the
//!   workspace-lint config for macros defined in external crates.
//!
//! Per macro, the effective reference set is `Layer 1 ∪ Layer 2` for
//! workspace-owned macros, else `Layer 3` for declared externals, else empty
//! (documented in `known_false_positives` fixtures).
//!
//! Plugins ([`crate::plugins::MacroBodyParser`]) are a fourth, orthogonal
//! mechanism: instead of declaring references statically, they parse macro
//! bodies on demand into structured ASTs (e.g. `dioxus-rsx` for `rsx!`).

pub mod annotation;
pub mod autodetect;
pub(crate) mod dispatch;
pub mod external;
