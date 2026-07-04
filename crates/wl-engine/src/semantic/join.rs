//! The cross-config half of the hash join (SPIKE §7 refinement).
//!
//! Every `[engine] configs` entry runs its own `cargo check` but they all
//! share ONE cargo target dir — so they are one compilation universe. A
//! `DefPathHash` minted under any config therefore identifies the same def
//! everywhere (it embeds `StableCrateId`), and joining a reference edge to its
//! target by that hash is exact across configs, not just within one.
//!
//! Why this is needed: a `+test` unit (or an integration-test crate, or a
//! bench) links its dependencies' **plain** rlibs, so its cross-crate
//! `to_key`s carry the *plain* generation of those defs — the generation
//! extracted only into the primary config's dir (cargo freshness never
//! re-extracts a fresh plain unit under a later config). Resolving such an
//! edge against the referring config's own `defs` alone misses; it must fall
//! back to this global index. The per-config verdict dimension is untouched:
//! [`super::assembly::Assembly`] translates a global hit to *its own* key for
//! the same identity before accumulating, so every count still lands under a
//! local def and the `(crate, def_path)` identity union decides as before.

use std::collections::BTreeMap;
use std::sync::Arc;

use wl_ir::IrFragment;

/// Stable `DefPathHash` → the cross-config identity (`crate::module::…::name`)
/// of the def it names, unioned over every config's fragments. Built once per
/// [`super::SemanticModel::assemble`] and shared (via [`Arc`]) by every
/// [`super::assembly::Assembly`] so each can resolve a foreign-generation edge
/// target to a known identity.
pub(super) struct IdentityIndex {
    identity_of: BTreeMap<String, String>,
}

impl IdentityIndex {
    /// Union every config's item keys into the global index. A `DefPathHash` is
    /// globally unique, so a key seen in two configs names the same def and the
    /// two identities agree by construction — first-write (matrix order) wins.
    pub(super) fn build(configs: &[(String, Vec<IrFragment>)]) -> Arc<Self> {
        let mut identity_of: BTreeMap<String, String> = BTreeMap::new();
        for (_, frags) in configs {
            for frag in frags {
                for it in &frag.items {
                    identity_of
                        .entry(it.key.clone())
                        .or_insert_with(|| it.path.join("::"));
                }
            }
        }
        Arc::new(Self { identity_of })
    }

    /// The identity of the def `key` names, if any config extracted it.
    pub(super) fn identity_of(&self, key: &str) -> Option<&str> {
        self.identity_of.get(key).map(String::as_str)
    }
}
