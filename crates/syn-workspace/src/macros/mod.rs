//! Macro reference tracking — the three layers that surface what items
//! a macro's expansion references, so consumers can avoid attributing
//! macro-mediated references incorrectly.
//!
//! - [`autodetect`] (Layer 1): scans `macro_rules!` bodies and proc-macro
//!   source for token-level identifiers.
//! - [`annotation`] (Layer 2): the paths a macro's expansion references,
//!   declared immediately before the definition — either an
//!   `expansion_uses!(...)` marker macro or the dependency-free
//!   `// workspace-syn: expansion-uses(...)` comment-directive form.
//! - [`external`] (Layer 3): declarative entries for macros defined in
//!   external crates — supplied by the caller (e.g. parsed from a config
//!   file) via [`crate::Workspace::register_external_macro_uses`].
//!
//! Per macro, the effective reference set is `Layer 1 ∪ Layer 2` for
//! workspace-owned macros, else `Layer 3` for declared externals, else
//! empty.
//!
//! Plugins (`crate::plugins::MacroLowerer`) are a fourth, orthogonal
//! mechanism: instead of declaring references statically, they parse
//! macro bodies on demand into structured ASTs (e.g. `dioxus-rsx` for
//! `rsx!`). The module walker dispatches to the lowerer registry via
//! `plugins::builtin_lowerers` / `plugins::claims_any`.

pub mod annotation;
pub mod autodetect;
pub mod external;
