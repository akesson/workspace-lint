//! Plugin: `#[serde(with = "mod")]` / `#[serde(crate = "mod")]` name a path in a
//! string literal no scan would otherwise see.
//!
//! serde's `with` contract requires the named module to expose `serialize` /
//! `deserialize`, so each fires three reference facts — the module plus those two
//! children (so `unused-pub` doesn't flag the helper fns). A path written absolute
//! (`::serde_with::…`) is credited verbatim; a relative one (`with = "routes"`) is
//! resolved against the module scope exactly like an ordinary reference.

use super::{Trigger, UsageAssertion, scan};
use crate::plugins::{Fact, LocalFactCtx, ResolverPlugin};

pub(crate) const SERDE_WITH: UsageAssertion = UsageAssertion {
    id: "serde-with",
    trigger: Trigger::AttrStringValue {
        attr: "serde",
        keys: &["with", "crate"],
        children: &["serialize", "deserialize"],
    },
    implies: &[],
    citation: "https://serde.rs/field-attrs.html#with",
};

pub(crate) struct SerdeWithPlugin;

impl ResolverPlugin for SerdeWithPlugin {
    fn local_facts(&self, item: &syn::Item, cx: &LocalFactCtx) -> Vec<Fact> {
        scan(&SERDE_WITH, "serde", item, cx)
    }
}
