//! Layer 2: explicit annotations on macro definitions.
//!
//! Today this layer recognises one form: an `expansion_uses!(...)` marker
//! macro placed next to a `macro_rules!` body to declare the paths the
//! macro's expansion will reference. The body is scanned identically to
//! Layer 1 autodetect.
//!
//! ```ignore
//! my_marker::expansion_uses!(serde::Serialize, chrono::DateTime);
//! macro_rules! my_macro { /* ... */ }
//! ```
//!
//! The marker-crate names recognized as the leading segment of an
//! `expansion_uses!` invocation are configurable via
//! [`crate::Workspace::load_with_options`]; by default the names
//! `workspace_syn` and `syn_workspace_marker` are accepted (these match
//! the historical defaults). Restricting the prefix avoids treating a
//! third-party `foo::expansion_uses!` as a Layer 2 annotation, which
//! would silently feed its body into the implicit-refs set.

/// Match `expansion_uses!` (unqualified) or `<crate>::expansion_uses!`
/// where the leading segment matches one of `marker_crates`. The
/// unqualified form always matches (it can't be confused with a
/// third-party macro because there's no prefix to attribute it to).
pub(crate) fn is_expansion_uses(path: &syn::Path, marker_crates: &[String]) -> bool {
    let segs: Vec<&syn::Ident> = path.segments.iter().map(|s| &s.ident).collect();
    match segs.as_slice() {
        [single] => *single == "expansion_uses",
        [krate, name] => {
            *name == "expansion_uses" && marker_crates.iter().any(|m| *krate == m.as_str())
        }
        _ => false,
    }
}
