// Glob import — Tier 1 skips glob bindings, but glob_targets_from_use
// records the prefix as a reference so `provider` isn't flagged.
use provider::prelude::*;

pub fn run() {
    helper();
}
