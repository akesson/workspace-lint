//! Resolver plugins — the resolver's unified extension point.
//!
//! A [`ResolverPlugin`] teaches the resolver about one macro, crate, or framework
//! whose reality the plain `syn` walk can't see. Each plugin contributes through up
//! to four hooks, all defaulted to no-ops, so a plugin implements only what it needs:
//!
//! - [`ResolverPlugin::claims_macro`] / [`ResolverPlugin::lower_macro`] — *walk
//!   control* over a class of macro *bodies*: run the baseline token scan, replace it
//!   with structured occurrences, or both (see [`Lowered`]). This is the only hook
//!   that steers traversal; the others emit data.
//! - [`ResolverPlugin::local_facts`] — [`Fact`]s derived from a single item during the
//!   walk (its attributes, its signature position). The builder-attribute exposure
//!   recognizer lives here ([`builder::BuilderAttrPlugin`]).
//! - [`ResolverPlugin::global_facts`] — [`Fact`]s derived from the whole resolved
//!   member set, after the walk: framework semantics a path resolution can't produce
//!   (e.g. a Dioxus component bound to a bare `Foo {}` `rsx!` invocation).
//!
//! A plugin only ever contributes one of **two** relations — a reference edge or a
//! public-signature exposure ([`Fact`]) — because those are the only two semantic
//! relations the model carries (no type inference, no trait solving). Each fact
//! carries [`Provenance`] (which plugin asserted it, and why) so a future `--explain`
//! can attribute it. A `Fact` can only ever *suppress* a finding (add a reference,
//! keep a type public), never create one — so recognition keys on shape, never on a
//! must-be-correct signal.
//!
//! **Internal extension point**, not public API: the trait, the site/context types,
//! and [`builtin_plugins`] are all `pub(crate)`; downstream consumers see only the
//! resolved model via [`crate::Workspace`].
//!
//! ## Adding a plugin
//!
//! 1. Define a struct and `impl ResolverPlugin for ...`, overriding only the hooks it
//!    needs; colocate unit tests in a `#[cfg(test)] mod tests` block. A plugin with
//!    non-trivial body parsing lives in its own `plugins/<name>/mod.rs` folder.
//! 2. **If it brings an extra crate dep**: mark that dep `optional = true`, add a
//!    `<name>` feature, and `#[cfg(feature = "<name>")]`-gate both the `mod <name>;`
//!    line and the [`builtin_plugins`] push.
//! 3. Append it to [`builtin_plugins`]. Macro claiming is first-match-wins, so order
//!    matters only among macro-claiming plugins; fact-only plugins are independent.

use std::collections::HashSet;
use std::path::Path;

use proc_macro2::TokenStream;

use crate::macros::annotation::is_expansion_uses;
use crate::resolve::use_tree::{Scope, UseBinding};
use crate::resolve::{Crate, Occurrence, ResolvedPath, SignatureExposure, SourceSpan};

pub(crate) mod assertions;
pub(crate) mod builder;
pub(crate) mod glob_imports;
pub(crate) mod macro_calls;
pub(crate) mod quote;

#[cfg(feature = "dioxus")]
pub(crate) mod dioxus_rsx;

// ---- macro-body lowering (walk control) ------------------------------------

/// A macro item encountered during the module walk.
pub(crate) struct MacroSite<'a> {
    /// `macro_rules! name { ... }` definition (as opposed to an invocation).
    pub is_macro_rules: bool,
    /// The macro path (`quote`, `dioxus::rsx`, the marker path for
    /// `expansion_uses!`, or `macro_rules` for a definition).
    pub path: &'a syn::Path,
    /// The macro body / argument token stream. Read only by structured lowerers
    /// (currently the dioxus `rsx!` plugin).
    #[cfg_attr(not(feature = "dioxus"), allow(dead_code))]
    pub tokens: &'a TokenStream,
    /// Marker crates that flag an `expansion_uses!` annotation.
    pub marker_crates: &'a [String],
}

impl MacroSite<'_> {
    /// The macro path as raw segment strings, for path-matching plugins.
    pub(crate) fn path_segments(&self) -> ResolvedPath {
        ResolvedPath::new(self.path.segments.iter().map(|s| s.ident.to_string()))
    }
}

/// Context for lowering. Carries the span to attach to structured occurrences —
/// the macro-invocation site, since the plugin AST doesn't expose per-ref spans.
pub(crate) struct LowerCtx {
    /// Read only by structured lowerers (currently the dioxus `rsx!` plugin).
    #[cfg_attr(not(feature = "dioxus"), allow(dead_code))]
    pub macro_span: Option<SourceSpan>,
}

/// What [`ResolverPlugin::lower_macro`] does with a claimed macro body.
pub(crate) enum Lowered {
    /// Run the baseline token scan over the body (the default macro behavior).
    TokenScan,
    /// Structured occurrences fully replace the scan. Reserved for a Layer-3
    /// external-macro follow-up; no built-in plugin emits it yet, but the dispatch
    /// handles it.
    #[allow(dead_code)]
    Exact(Vec<Occurrence>),
    /// Run the baseline scan AND add these structured occurrences. Currently only the
    /// dioxus `rsx!` plugin emits this.
    #[cfg_attr(not(feature = "dioxus"), allow(dead_code))]
    ScanPlus(Vec<Occurrence>),
}

// ---- contributed facts ------------------------------------------------------

/// Which plugin asserted a [`Fact`], and why. Subsumes the former per-reference
/// `Origin::Asserted { rule }` tag (the Tier-H assertions are now
/// [`assertions`]-module plugins) and extends provenance to exposures (which carry none
/// on their own). Recorded into the workspace's provenance side table for a future
/// `--explain`; it never affects whether a finding fires.
#[allow(dead_code)] // fields read by a future `--explain`; populated + tested now.
#[derive(Debug, Clone)]
pub(crate) struct Provenance {
    /// Stable plugin id — `"typed_builder"`, `"dioxus"`, `"macro_calls"`.
    pub plugin: &'static str,
    /// The specific rule/trigger within the plugin — `"build_method.into"`.
    pub rule: &'static str,
    /// Where the triggering syntax sits (attribute / macro site), for `--explain`.
    pub trigger: Option<SourceSpan>,
}

/// One reference edge a plugin discovered: crate `from` (code-form name) references
/// the canonical item `to`. Folded into the workspace's per-crate reference set, so it
/// flows through re-export canonicalization and the `referring_crates` index exactly
/// like a code reference.
pub(crate) struct ContributedRef {
    pub from: String,
    pub to: ResolvedPath,
    /// True when the referencing site lives in a *sibling target* of its package
    /// (integration test, bench, example, non-primary bin) — those link the package's
    /// lib as an external crate, so the referenced item must stay `pub`. Routed into
    /// `Workspace::sibling_target_refs` alongside the ordinary merge. Plugins that
    /// don't track target provenance leave this `false`.
    pub via_sibling_target: bool,
}

/// The two — and only two — semantic relations a plugin can contribute, each carrying
/// [`Provenance`]. A `Fact` can only *suppress* a finding (mark a use, keep a type
/// public), never create one.
pub(crate) enum Fact {
    /// A reference edge — feeds the "what is used" relation.
    Reference {
        edge: ContributedRef,
        by: Provenance,
    },
    /// A public-signature exposure — feeds the "what must stay `pub`" relation.
    Exposure {
        exp: SignatureExposure,
        by: Provenance,
    },
}

impl Fact {
    /// Reduce this fact to its provenance record — target canonical path, which
    /// relation, and which plugin asserted it. Folded into the workspace's
    /// [`Workspace::fact_provenance`](crate::Workspace) side table at both fold
    /// sites, alongside the fact's effect on the reference / exposure indices.
    pub(crate) fn provenance(&self) -> ProvenancedFact {
        let (path, kind, by) = match self {
            Fact::Reference { edge, by } => (edge.to.clone(), FactKind::Reference, by.clone()),
            Fact::Exposure { exp, by } => (exp.canonical.clone(), FactKind::Exposure, by.clone()),
        };
        ProvenancedFact { path, kind, by }
    }
}

/// Which relation a [`ProvenancedFact`] recorded.
#[allow(dead_code)] // read by a future `--explain`; populated + tested now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FactKind {
    Reference,
    Exposure,
}

/// A contributed [`Fact`] reduced to its target canonical path + [`Provenance`], for
/// the workspace-level provenance side table. Populated as facts fold into the
/// reference / exposure indices; read by a future `--explain`.
#[allow(dead_code)] // fields read by a future `--explain`; populated + tested now.
#[derive(Debug, Clone)]
pub(crate) struct ProvenancedFact {
    pub path: ResolvedPath,
    pub kind: FactKind,
    pub by: Provenance,
}

/// Read-only resolution context handed to [`ResolverPlugin::local_facts`] — the same
/// borrows the signature walk threads, plus the source file (for trigger spans). A
/// plugin resolves a written path to a canonical exposure via
/// [`crate::resolve::module_tree::signature::record_exposed_type`].
pub(crate) struct LocalFactCtx<'a> {
    pub scope: &'a Scope,
    pub siblings: &'a HashSet<String>,
    pub use_bindings: &'a [UseBinding],
    pub parent_canonical: &'a ResolvedPath,
    pub file: &'a Path,
}

impl LocalFactCtx<'_> {
    /// Resolve a syntactic span to a [`SourceSpan`] anchored in this item's file, for
    /// a [`Fact`]'s [`Provenance::trigger`].
    pub(crate) fn span(&self, span: proc_macro2::Span) -> Option<SourceSpan> {
        Some(crate::resolve::module_tree::span_to_source_span(
            self.file, span,
        ))
    }
}

// ---- the trait + registry ---------------------------------------------------

/// One plugin per macro / crate / framework. Object-safe; built fresh by each
/// [`builtin_plugins`] call. Every hook defaults to a no-op.
pub(crate) trait ResolverPlugin: Send + Sync {
    /// Walk-control (cheap predicate): does this plugin own this macro site? Kept
    /// separate from [`lower_macro`](ResolverPlugin::lower_macro) so the code-path
    /// pass's claim guard never pays the lowering cost (e.g. rsx parsing).
    fn claims_macro(&self, _site: &MacroSite) -> bool {
        false
    }
    /// Walk-control: lower a claimed macro body. Called only when
    /// [`claims_macro`](ResolverPlugin::claims_macro) returned true for this plugin.
    fn lower_macro(&self, _site: &MacroSite, _cx: &LowerCtx) -> Lowered {
        Lowered::TokenScan
    }
    /// Facts derived from one item during the walk (its attrs / signature surface).
    fn local_facts(&self, _item: &syn::Item, _cx: &LocalFactCtx) -> Vec<Fact> {
        Vec::new()
    }
    /// Facts derived from the whole resolved member set, after the walk. Independent
    /// pure contributors — never mutate the model, never see each other; the single
    /// writer in `Workspace::load_with_options` unions every plugin's edges.
    fn global_facts(&self, _crates: &[Crate]) -> Vec<Fact> {
        Vec::new()
    }
}

/// `macro_rules! name { ... }` bodies — token-scanned at the definition scope.
struct MacroRulesLowerer;

impl ResolverPlugin for MacroRulesLowerer {
    fn claims_macro(&self, site: &MacroSite) -> bool {
        site.is_macro_rules
    }

    fn lower_macro(&self, _site: &MacroSite, _cx: &LowerCtx) -> Lowered {
        Lowered::TokenScan
    }
}

/// `expansion_uses!(path, ...)` annotations — arguments token-scanned.
struct AnnotationLowerer;

impl ResolverPlugin for AnnotationLowerer {
    fn claims_macro(&self, site: &MacroSite) -> bool {
        !site.is_macro_rules && is_expansion_uses(site.path, site.marker_crates)
    }

    fn lower_macro(&self, _site: &MacroSite, _cx: &LowerCtx) -> Lowered {
        Lowered::TokenScan
    }
}

/// All built-in resolver plugins. Macro-claiming is first-match-wins, so the
/// macro-claiming plugins precede the fact-only ones; among the latter, order is
/// irrelevant (the workspace folds their facts into a set).
pub(crate) fn builtin_plugins() -> Vec<Box<dyn ResolverPlugin>> {
    // `mut` only needed when a feature-gated plugin is enabled.
    #[allow(unused_mut)]
    let mut v: Vec<Box<dyn ResolverPlugin>> = vec![
        Box::new(MacroRulesLowerer),
        Box::new(AnnotationLowerer),
        Box::new(quote::QuoteLowerer),
        Box::new(macro_calls::MacroCallPass),
        Box::new(glob_imports::GlobImportPass),
        Box::new(builder::BuilderAttrPlugin),
        // Tier-H usage assertions — one plugin per crate whose macro-expansion
        // contract names a path no source scan can see.
        Box::new(assertions::strum::StrumPlugin),
        Box::new(assertions::serde::SerdeWithPlugin),
        Box::new(assertions::wasm_bindgen::WasmBindgenTestPlugin),
    ];
    #[cfg(feature = "dioxus")]
    v.push(Box::new(dioxus_rsx::DioxusPlugin));
    v
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

    /// Does any built-in plugin claim this macro site? (Mirrors the code-path pass's
    /// inline claim guard.)
    fn claims_any(site: &MacroSite) -> bool {
        builtin_plugins().iter().any(|p| p.claims_macro(site))
    }

    #[test]
    fn registry_is_nonempty() {
        // Ships at least the macro-rules / annotation / quote lowerers, the two core
        // resolve passes, and the builder-attr plugin.
        assert!(builtin_plugins().len() >= 6);
    }

    #[test]
    fn macro_rules_definition_is_claimed_and_token_scanned() {
        let path = syn_path("macro_rules");
        let tokens = TokenStream::new();
        let s = site(true, &path, &tokens);
        assert!(claims_any(&s));
        let plugins = builtin_plugins();
        let claimer = plugins
            .iter()
            .find(|p| p.claims_macro(&s))
            .expect("a plugin claims macro_rules");
        assert!(matches!(
            claimer.lower_macro(&s, &LowerCtx { macro_span: None }),
            Lowered::TokenScan
        ));
    }

    #[test]
    fn quote_invocation_is_claimed() {
        let path = syn_path("quote");
        let tokens = TokenStream::new();
        assert!(claims_any(&site(false, &path, &tokens)));
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
}
