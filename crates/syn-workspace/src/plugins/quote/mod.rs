//! Lowerer: `quote!` / `quote::quote!` bodies.
//!
//! `quote!` bodies are token streams the proc-macro emits as Rust source at
//! expansion time; their contents reference items the caller sees at the
//! expansion site, which the baseline token scan (multi-segment path tokens,
//! resolved through the call-site scope) handles. So this plugin simply
//! [`claims_macro`](ResolverPlugin::claims_macro)s the `quote!` paths and asks for a
//! [`Lowered::TokenScan`] — no structured extraction, and no fake-empty
//! reference list to gate the scan.

use crate::plugins::{LowerCtx, Lowered, MacroSite, ResolverPlugin};

/// Built-in lowerer for `quote!` and `quote::quote!` invocations.
pub(crate) struct QuoteLowerer;

impl ResolverPlugin for QuoteLowerer {
    fn claims_macro(&self, site: &MacroSite) -> bool {
        if site.is_macro_rules {
            return false;
        }
        // Match bare `quote!` and `quote::quote!`. Other suffixes
        // (`quote_spanned!`, `format_ident!`) intentionally don't match —
        // their body semantics differ.
        match site.path_segments().segments() {
            [single] => single == "quote",
            [a, b] => a == "quote" && b == "quote",
            _ => false,
        }
    }

    fn lower_macro(&self, _site: &MacroSite, _cx: &LowerCtx) -> Lowered {
        Lowered::TokenScan
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proc_macro2::TokenStream;

    fn site(path: &str) -> (syn::Path, TokenStream) {
        (
            syn::parse_str(path).expect("valid path"),
            TokenStream::new(),
        )
    }

    fn claims(path: &str) -> bool {
        let (p, t) = site(path);
        QuoteLowerer.claims_macro(&MacroSite {
            is_macro_rules: false,
            path: &p,
            tokens: &t,
            marker_crates: &[],
        })
    }

    #[test]
    fn claims_bare_and_qualified_quote() {
        assert!(claims("quote"));
        assert!(claims("quote::quote"));
    }

    #[test]
    fn does_not_claim_unrelated_or_macro_rules() {
        assert!(!claims("lazy_static"));
        assert!(!claims("quote_spanned"));
        // A `macro_rules!` definition is owned by MacroRulesLowerer.
        let (p, t) = site("quote");
        assert!(!QuoteLowerer.claims_macro(&MacroSite {
            is_macro_rules: true,
            path: &p,
            tokens: &t,
            marker_crates: &[],
        }));
    }
}
