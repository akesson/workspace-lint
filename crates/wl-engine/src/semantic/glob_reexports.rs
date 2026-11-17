//! Which cascade deletions would empty a `pub use m::*` glob re-export.
//!
//! A glob that re-exports nothing trips `unused_imports` (and one left with
//! only `pub(crate)` items trips rustc's "glob import doesn't reexport
//! anything with visibility `pub`") — a `-D warnings` failure on the fixed
//! tree. Deleting SOME items behind a glob is harmless: it keeps
//! re-exporting the rest.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use super::SemanticModel;
use super::removal::RemovalSet;

impl SemanticModel {
    /// The items of `trial_new` (deleted on top of the rest of `trial`)
    /// behind a glob re-export that `trial` would leave empty — every
    /// public child of the re-exported module, in any config, is in
    /// `trial` — each mapped to that module's path.
    ///
    /// Errs toward emptiness, never away from it: what the glob re-exports
    /// besides the module's own public defs (a `pub use` inside it, an item
    /// under a cfg no config compiles) isn't counted, and a non-glob
    /// `pub use m;` is judged like `pub use m::*` (the assembler can't tell
    /// them apart) — both only keep an item that could have gone.
    pub fn glob_emptied(
        &self,
        trial: &RemovalSet,
        trial_new: &[String],
    ) -> BTreeMap<String, String> {
        let mut children: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for (_, asm) in &self.configs {
            for (module, kids) in &asm.glob_children {
                children
                    .entry(module)
                    .or_default()
                    .extend(kids.iter().map(String::as_str));
            }
        }
        let new: HashSet<&str> = trial_new.iter().map(String::as_str).collect();
        let mut out = BTreeMap::new();
        for (module, kids) in children {
            if !kids.iter().all(|k| trial.contains_id(k)) {
                continue;
            }
            for kid in kids.into_iter().filter(|k| new.contains(k)) {
                out.entry(kid.to_owned())
                    .or_insert_with(|| module.to_owned());
            }
        }
        out
    }
}
