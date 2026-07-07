//! Dispatch reachability: how a def can be *provably in use* without any
//! direct edge landing on it. Hosted next to [`Assembly`] (like the clippy
//! and `dead_code` replay guards in `clippy_guard`) so the assembly stays the
//! index builder and this stays the judgment.

use std::collections::BTreeMap;

use super::assembly::Assembly;
use super::def_info::{DefInfo, Reach};

impl Assembly {
    /// Reachability of a candidate def under this single-config assembly.
    /// Direct use-site first; then, for a trait-impl item, dispatch expansion
    /// off its `trait_item`: an external trait is a root (invisible dispatch),
    /// an internal one is reached iff its method is dispatched anywhere.
    /// Export-shaped attributes root a def unconditionally (linker-visible,
    /// no Rust referrer possible). Module-level and inherent-impl items carry
    /// no `trait_item`, so they fall through to Direct-or-Unreached.
    pub fn reach_of(&self, key: &str, def: &DefInfo) -> Reach {
        self.reach_with(key, def, &self.degrees.in_degree)
    }

    /// [`Assembly::reach_of`] against an explicit in-degree map — the
    /// unused-pub `--fix` cascade passes a removal overlay's recomputed
    /// in-degree so a def whose only referrers were deleted this pass reads as
    /// `Unreached` (dispatch reachability follows too: a trait item loses its
    /// `InternalDispatch` when its last dispatcher is removed).
    ///
    /// The dispatch lookups stay **local** (`self.defs` / `in_degree`, not the
    /// global cross-config index): a global miss falls through to
    /// `ExternalDispatch` = reached, the sound (never-a-false-lead) direction,
    /// and trait-impl items are judged but never *flagged* by the lint anyway;
    /// the identity union already ORs in the primary config's dispatch verdict.
    pub(super) fn reach_with(
        &self,
        key: &str,
        def: &DefInfo,
        in_degree: &BTreeMap<String, usize>,
    ) -> Reach {
        // Export roots win over Direct: a proc-macro entry point picks up a
        // phantom intra-crate edge from the compiler-synthesized `_DECLS`
        // registration, and Direct would let that edge read as "only used
        // inside the crate" — narrowing a `#[proc_macro_derive]` fn is a hard
        // compile error.
        if def.export_root {
            return Reach::ExportRoot;
        }
        if in_degree.contains_key(key) {
            return Reach::Direct;
        }
        if let Some(ti) = &def.trait_item {
            if !self.defs.contains_key(ti) {
                return Reach::ExternalDispatch; // trait defined outside the workspace
            }
            if in_degree.contains_key(ti) {
                return Reach::InternalDispatch; // internal trait method is dispatched
            }
        }
        Reach::Unreached
    }
}
