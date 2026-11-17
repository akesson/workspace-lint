//! Which cascade deletions would empty a `pub use m::*` glob re-export.
//!
//! A glob that re-exports nothing trips `unused_imports` (and one left with
//! only `pub(crate)` items trips rustc's "glob import doesn't reexport
//! anything with visibility `pub`") — a `-D warnings` failure on the fixed
//! tree. Deleting SOME items behind a glob is harmless: it keeps
//! re-exporting the rest.

use std::collections::{BTreeMap, HashSet};

use super::SemanticModel;
use super::removal::RemovalSet;

impl SemanticModel {
    /// The items of `trial_new` (deleted on top of the rest of `trial`)
    /// behind a `pub use m::*` glob that `trial` would leave empty, each
    /// mapped to the glob's module path. Judged per config: a glob empties
    /// if, in ANY config, every public child of `m` that config compiles is
    /// in `trial` — a `#[cfg(test)]` survivor keeps it non-empty only under
    /// `cargo test`, and the `cargo build` glob still empties.
    ///
    /// Errs toward emptiness, never away from it: what the glob re-exports
    /// besides the module's own public defs (a `pub use` inside it) isn't
    /// counted — that only keeps an item that could have gone.
    pub fn glob_emptied(
        &self,
        trial: &RemovalSet,
        trial_new: &[String],
    ) -> BTreeMap<String, String> {
        let new: HashSet<&str> = trial_new.iter().map(String::as_str).collect();
        let mut out = BTreeMap::new();
        for (_, asm) in &self.configs {
            for (module, kids) in &asm.glob_children {
                if !kids.iter().all(|k| trial.contains_id(k)) {
                    continue;
                }
                for kid in kids.iter().filter(|k| new.contains(k.as_str())) {
                    out.entry(kid.clone()).or_insert_with(|| module.clone());
                }
            }
        }
        out
    }
}
