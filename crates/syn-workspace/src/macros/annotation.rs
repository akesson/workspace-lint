//! Layer 2: explicit annotations on macro definitions.
//!
//! Today this layer recognises one form: the `expansion_uses!(...)` marker
//! macro shipped by `syn-workspace-marker`, which sits next to a
//! `macro_rules!` body and declares the paths the macro's expansion will
//! reference. The body is scanned identically to Layer 1 autodetect.
//!
//! ```ignore
//! workspace_syn::expansion_uses!(serde::Serialize, chrono::DateTime);
//! macro_rules! my_macro { /* ... */ }
//! ```
//!
//! A future comment-directive form
//! (`// workspace-syn: expansion-uses(...)`) is documented but not yet
//! implemented — tracked in `known_false_*` fixtures.

/// Match `expansion_uses!` (unqualified) or `<crate>::expansion_uses!` where
/// the leading segment is the `syn-workspace-marker` crate (typically
/// imported as `workspace_syn` or `syn_workspace_marker`). Restricting the
/// prefix avoids treating a third-party `foo::expansion_uses!` as a Layer 2
/// annotation, which would silently feed its body into the implicit-refs
/// set and corrupt visibility/unused-pub findings.
pub(crate) fn is_expansion_uses(path: &syn::Path) -> bool {
    let segs: Vec<&syn::Ident> = path.segments.iter().map(|s| &s.ident).collect();
    match segs.as_slice() {
        [single] => *single == "expansion_uses",
        [krate, name] => {
            *name == "expansion_uses"
                && (*krate == "workspace_syn" || *krate == "syn_workspace_marker")
        }
        _ => false,
    }
}
