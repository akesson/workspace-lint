//! Macro-body lowerers (internal).
//!
//! Macro lowering is the single Phase-A extension point of the resolver:
//! everything else (regular-code path scan, use-trees, mod resolution,
//! extern-crate) is core. A [`MacroLowerer`] claims a class of macro sites and
//! lowers each into [`Lowered`] — either a request to run the baseline token
//! scan over the body ([`Lowered::TokenScan`]), structured occurrences that
//! replace the scan ([`Lowered::Exact`]), or both ([`Lowered::ScanPlus`]).
//!
//! **This is an internal extension point**, not a public API. The trait, the
//! site/context types, and the registry are all `pub(crate)`; downstream
//! consumers see only the resolved references via [`crate::Workspace`].
//!
//! ## Adding a built-in lowerer
//!
//! 1. Define a struct and `impl MacroLowerer for ...` — colocate unit tests in
//!    a `#[cfg(test)] mod tests` block. Path-matching lowerers live in their own
//!    `plugins/<name>/mod.rs` folder.
//! 2. **If it brings an extra crate dep**: mark that dep `optional = true` in
//!    `Cargo.toml`, add a `<name>` feature, and `#[cfg(feature = "<name>")]`-gate
//!    both the `mod <name>;` line and the [`builtin_lowerers`] push.
//! 3. Append it to [`builtin_lowerers`] (claim-priority order).

use proc_macro2::TokenStream;

use crate::macros::annotation::is_expansion_uses;
use crate::resolve::{Crate, Occurrence, ResolvedPath, SourceSpan};

pub(crate) mod quote;

#[cfg(feature = "dioxus")]
pub(crate) mod dioxus_rsx;

/// A macro item encountered during the module walk.
pub(crate) struct MacroSite<'a> {
    /// `macro_rules! name { ... }` definition (as opposed to an invocation).
    pub is_macro_rules: bool,
    /// The macro path (`quote`, `dioxus::rsx`, the marker path for
    /// `expansion_uses!`, or `macro_rules` for a definition).
    pub path: &'a syn::Path,
    /// The macro body / argument token stream. Read only by structured lowerers
    /// (currently the dioxus `rsx!` lowerer).
    #[cfg_attr(not(feature = "dioxus"), allow(dead_code))]
    pub tokens: &'a TokenStream,
    /// Marker crates that flag an `expansion_uses!` annotation.
    pub marker_crates: &'a [String],
}

impl MacroSite<'_> {
    /// The macro path as raw segment strings, for path-matching lowerers.
    pub(crate) fn path_segments(&self) -> ResolvedPath {
        ResolvedPath::new(self.path.segments.iter().map(|s| s.ident.to_string()))
    }
}

/// Context for lowering. Carries the span to attach to structured occurrences —
/// the macro-invocation site, since the plugin AST doesn't expose per-ref spans.
pub(crate) struct LowerCtx {
    /// Read only by structured lowerers (currently the dioxus `rsx!` lowerer).
    #[cfg_attr(not(feature = "dioxus"), allow(dead_code))]
    pub macro_span: Option<SourceSpan>,
}

/// What a lowerer does with a claimed macro body.
pub(crate) enum Lowered {
    /// Run the baseline token scan over the body (the old Layer-1 behavior).
    TokenScan,
    /// Structured occurrences fully replace the scan. Reserved for Layer-3
    /// external macros (a test-gated follow-up); no built-in lowerer emits it
    /// yet, but the dispatch handles it.
    #[allow(dead_code)]
    Exact(Vec<Occurrence>),
    /// Run the baseline scan AND add these structured occurrences. Currently
    /// only the dioxus `rsx!` lowerer emits this.
    #[cfg_attr(not(feature = "dioxus"), allow(dead_code))]
    ScanPlus(Vec<Occurrence>),
}

/// A pluggable lowerer for a class of macro bodies — the resolver's only
/// Phase-A extension point.
pub(crate) trait MacroLowerer: Send + Sync {
    /// Whether this lowerer owns the given macro site.
    fn claims(&self, site: &MacroSite) -> bool;
    /// Lower the claimed site to its [`Lowered`] behavior.
    fn lower(&self, site: &MacroSite, cx: &LowerCtx) -> Lowered;
}

/// `macro_rules! name { ... }` bodies — token-scanned at the definition scope.
struct MacroRulesLowerer;

impl MacroLowerer for MacroRulesLowerer {
    fn claims(&self, site: &MacroSite) -> bool {
        site.is_macro_rules
    }

    fn lower(&self, _site: &MacroSite, _cx: &LowerCtx) -> Lowered {
        Lowered::TokenScan
    }
}

/// `expansion_uses!(path, ...)` annotations — arguments token-scanned.
struct AnnotationLowerer;

impl MacroLowerer for AnnotationLowerer {
    fn claims(&self, site: &MacroSite) -> bool {
        !site.is_macro_rules && is_expansion_uses(site.path, site.marker_crates)
    }

    fn lower(&self, _site: &MacroSite, _cx: &LowerCtx) -> Lowered {
        Lowered::TokenScan
    }
}

/// All built-in macro lowerers, in claim-priority order.
pub(crate) fn builtin_lowerers() -> Vec<Box<dyn MacroLowerer>> {
    // `mut` only needed when a feature-gated lowerer is enabled.
    #[allow(unused_mut)]
    let mut v: Vec<Box<dyn MacroLowerer>> = vec![
        Box::new(MacroRulesLowerer),
        Box::new(AnnotationLowerer),
        Box::new(quote::QuoteLowerer),
    ];
    #[cfg(feature = "dioxus")]
    v.push(Box::new(dioxus_rsx::DioxusRsxLowerer));
    v
}

/// Whether any built-in lowerer claims this macro site — the single source of
/// truth for "was this macro handled in the macro pass?", used by the
/// code-path pass to avoid double-counting macro bodies as regular code.
pub(crate) fn claims_any(site: &MacroSite) -> bool {
    builtin_lowerers().iter().any(|l| l.claims(site))
}

/// One reference edge a [`ResolvePass`] discovered: crate `from` (code-form
/// name) references the canonical item `to`. Folded into the workspace's
/// per-crate reference set, so it flows through re-export canonicalization and
/// the `referring_crates` index exactly like a code reference.
pub(crate) struct ContributedRef {
    pub from: String,
    pub to: ResolvedPath,
}

/// A Phase-B resolution contributor — the resolver's *semantic* extension point,
/// symmetric to [`MacroLowerer`] (Phase A). Where a `MacroLowerer` lowers one
/// macro body in isolation, a `ResolvePass` sees the whole resolved member set
/// and contributes references that pure path resolution structurally can't
/// produce (framework semantics — e.g. a Dioxus `#[component]` bound to a bare
/// `Foo {}` `rsx!` invocation, a reference path resolution alone can't see).
///
/// Passes are **independent pure contributors** (a ROADMAP non-goal makes this a
/// hard rule): each reads the member crates and returns edges; it never mutates
/// the model and is never aware of another pass. The single writer in
/// `Workspace::load_with_options` unions every pass's edges into a set, so the
/// merged result is order-independent by construction.
pub(crate) trait ResolvePass: Send + Sync {
    /// Inspect the resolved member crates; return extra reference edges.
    fn contribute(&self, crates: &[Crate]) -> Vec<ContributedRef>;
}

/// All built-in Phase-B resolve passes. Default-empty until a framework feature
/// is enabled — the hook is a genuine no-op otherwise (ROADMAP Phase 4: "the
/// hook stays empty until then").
pub(crate) fn builtin_resolve_passes() -> Vec<Box<dyn ResolvePass>> {
    #[cfg(feature = "dioxus")]
    let passes: Vec<Box<dyn ResolvePass>> = vec![Box::new(dioxus_rsx::DioxusComponentPass)];
    #[cfg(not(feature = "dioxus"))]
    let passes: Vec<Box<dyn ResolvePass>> = Vec::new();
    passes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn syn_path(s: &str) -> syn::Path {
        syn::parse_str(s).expect("valid path")
    }

    fn site<'a>(
        is_macro_rules: bool,
        path: &'a syn::Path,
        tokens: &'a TokenStream,
    ) -> MacroSite<'a> {
        MacroSite {
            is_macro_rules,
            path,
            tokens,
            marker_crates: &[],
        }
    }

    #[test]
    fn trait_is_object_safe() {
        let lowerers = builtin_lowerers();
        assert!(
            !lowerers.is_empty(),
            "ships at least macro_rules + annotation + quote"
        );
    }

    #[test]
    fn macro_rules_definition_is_claimed_and_token_scanned() {
        let path = syn_path("macro_rules");
        let tokens = TokenStream::new();
        let s = site(true, &path, &tokens);
        assert!(claims_any(&s));
        let lowerer = builtin_lowerers();
        let claimer = lowerer
            .iter()
            .find(|l| l.claims(&s))
            .expect("a lowerer claims macro_rules");
        assert!(matches!(
            claimer.lower(&s, &LowerCtx { macro_span: None }),
            Lowered::TokenScan
        ));
    }

    #[test]
    fn quote_invocation_token_scans() {
        let path = syn_path("quote");
        let tokens = TokenStream::new();
        let s = site(false, &path, &tokens);
        assert!(claims_any(&s));
    }

    #[test]
    fn unrelated_macro_is_unclaimed() {
        let path = syn_path("lazy_static");
        let tokens = TokenStream::new();
        assert!(!claims_any(&site(false, &path, &tokens)));
    }

    #[cfg(feature = "dioxus")]
    #[test]
    fn rsx_invocation_is_claimed() {
        let path = syn_path("rsx");
        let tokens = TokenStream::new();
        assert!(claims_any(&site(false, &path, &tokens)));
        let qualified = syn_path("dioxus::rsx");
        assert!(claims_any(&site(false, &qualified, &tokens)));
    }

    #[cfg(feature = "dioxus")]
    #[test]
    fn resolve_passes_registered_with_dioxus() {
        // The Phase B hook ships the Dioxus component pass when the feature is on;
        // it is default-empty (a no-op) otherwise.
        assert!(!builtin_resolve_passes().is_empty());
    }
}
