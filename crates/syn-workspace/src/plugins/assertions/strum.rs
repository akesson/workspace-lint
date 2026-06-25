//! Plugin: a strum derive implies a reference to the `strum` runtime crate.
//!
//! A `#[derive(EnumString)]` (etc.) expands to code that references `strum`, which no
//! source path names. `strum_macros` (the proc-macro crate) is credited separately by
//! the `use strum_macros::…` / `#[derive(strum_macros::…)]` that names it; only `strum`
//! is invisible to parsing. Distinctive idents only — `Display` and `ToString` are
//! deliberately omitted (a bare `#[derive(Display)]` is ambiguous with
//! `derive_more::Display`), so a crate deriving *only* strum's `Display` unqualified
//! won't be covered; the qualified `#[derive(strum::Display)]` form still fires via
//! `crates`.

use super::{Trigger, UsageAssertion, scan};
use crate::plugins::{Fact, LocalFactCtx, ResolverPlugin};

pub(crate) const STRUM: UsageAssertion = UsageAssertion {
    id: "strum-derive",
    trigger: Trigger::DeriveIdent {
        idents: &[
            "AsRefStr",
            "EnumCount",
            "EnumDiscriminants",
            "EnumIs",
            "EnumIter",
            "EnumMessage",
            "EnumProperty",
            "EnumString",
            "EnumTryAs",
            "EnumVariantNames", // legacy (pre-0.26) name of VariantNames.
            "FromRepr",
            "IntoStaticStr",
            "VariantArray",
            "VariantNames",
        ],
        crates: &["strum", "strum_macros"],
    },
    implies: &["strum"],
    // "This crate only contains derive macros for use with the strum crate."
    citation: "https://docs.rs/strum_macros/latest/strum_macros/",
};

pub(crate) struct StrumPlugin;

impl ResolverPlugin for StrumPlugin {
    fn local_facts(&self, item: &syn::Item, cx: &LocalFactCtx) -> Vec<Fact> {
        scan(&STRUM, "strum", item, cx)
    }
}
