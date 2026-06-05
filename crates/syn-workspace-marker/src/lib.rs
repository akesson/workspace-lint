//! Marker macros for annotating macro expansions for `syn-workspace`.
//!
//! `syn-workspace` cannot expand `macro_rules!` or proc-macros — it sees only
//! the macro definition and the call sites. This crate provides annotations
//! that let macro authors declare which items their macro's expansion
//! references, so downstream lints (`unused-deps`, `unused-pub`, architecture
//! rules) don't produce false positives for items only reached through macro
//! expansion.
//!
//! Add this crate as a dependency, typically renamed to `workspace_syn`:
//!
//! ```toml
//! [dependencies]
//! workspace_syn = { package = "syn-workspace-marker", version = "0.1" }
//! ```
//!
//! Then annotate macro definitions with the paths they reference at expansion:
//!
//! ```ignore
//! workspace_syn::expansion_uses!(serde::Serialize, chrono::DateTime);
//! macro_rules! my_macro { /* ... */ }
//! ```
//!
//! Or, for callers who prefer comment directives (the form mirrors the
//! existing `# workspace-lint: allow(...)` style and requires no dependency):
//!
//! ```ignore
//! // workspace-syn: expansion-uses(serde::Serialize, chrono::DateTime)
//! macro_rules! my_macro { /* ... */ }
//! ```
//!
//! Both forms are parsed by `syn-workspace` at workspace-walk time, and should
//! immediately precede the item they annotate.

/// Declare paths referenced by a macro's expansion.
///
/// Expands to nothing at compile time. `syn-workspace` reads the invocation
/// out of the source tree and associates it with the next item in the file.
///
/// Accepts a comma-separated list of paths; trailing comma optional.
#[macro_export]
macro_rules! expansion_uses {
    ($($tt:tt)*) => {};
}

#[cfg(test)]
mod tests {
    // The macro expands to nothing — these tests prove the pattern accepts
    // the documented invocation shapes without requiring any runtime
    // behavior. Real parsing of the annotation is exercised in
    // `syn-workspace`'s integration tests against fixture workspaces.

    #[test]
    fn accepts_single_path() {
        crate::expansion_uses!(serde::Serialize);
    }

    #[test]
    fn accepts_comma_list() {
        crate::expansion_uses!(serde::Serialize, chrono::DateTime);
    }

    #[test]
    fn accepts_trailing_comma() {
        crate::expansion_uses!(serde::Serialize, chrono::DateTime,);
    }

    #[test]
    fn accepts_empty() {
        crate::expansion_uses!();
    }

    #[test]
    fn accepts_external_crate_paths() {
        crate::expansion_uses!(tokio::runtime::Builder, sqlx::query::Query);
    }
}
