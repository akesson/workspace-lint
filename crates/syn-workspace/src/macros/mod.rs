//! Macro reference tracking — the three layers that surface what items
//! a macro's expansion references, so consumers can avoid attributing
//! macro-mediated references incorrectly.
//!
//! - [`autodetect`] (Layer 1): scans `macro_rules!` bodies and proc-macro
//!   source for token-level identifiers.
//! - [`annotation`] (Layer 2): parses `expansion_uses!(...)` invocations
//!   that appear immediately before a macro definition. A future comment-
//!   directive form is on the roadmap.
//! - [`external`] (Layer 3): declarative entries for macros defined in
//!   external crates — supplied by the caller (e.g. parsed from a config
//!   file) via [`crate::Workspace::register_external_macro_uses`].
//!
//! Per macro, the effective reference set is `Layer 1 ∪ Layer 2` for
//! workspace-owned macros, else `Layer 3` for declared externals, else
//! empty.
//!
//! Plugins (`crate::plugins::MacroBodyParser`) are a fourth, orthogonal
//! mechanism: instead of declaring references statically, they parse
//! macro bodies on demand into structured ASTs (e.g. `dioxus-rsx` for
//! `rsx!`). The walker dispatches to them via `plugins::matches` and
//! `plugins::refs`.

pub mod annotation;
pub mod autodetect;
pub mod external;
