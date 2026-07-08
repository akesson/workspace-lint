//! P4: a glob kept alive ONLY by trait-method syntax. `Widget` is named by a
//! full written path (not through the glob); `.shout()` resolves because the
//! glob puts `Shout` in scope — visible only as a `trait_scope` edge
//! (typeck's `used_trait_imports`), never in the glob_map.
use crate::prelude::*;

pub fn call() -> &'static str {
    crate::prelude::Widget.shout()
}
