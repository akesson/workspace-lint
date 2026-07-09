// KNOWN FALSE NEGATIVE (orphan-file) — `all(unix, windows)` holds on no target,
// so `never_built.rs` is dead on *every* platform, not merely uncompiled by this
// matrix. No config can ever open it, so rustc's `loaded_files` cannot see it;
// the fast tier NAMES it, so the lint downgrades to a coverage gap rather than
// calling it an orphan.
//
// This is the accepted cost of zero false positives: the lint cannot tell an
// unsatisfiable cfg from a merely-undeclared one, and would rather stay silent
// than tell you to delete a live platform module. Contrast
// `true_negatives/cfg_gated_module_outside_matrix`, whose file *is* live under a
// config the user could declare — there the gap is the whole point.
//
// If the lint ever learns to evaluate cfg predicates for satisfiability, this
// case starts reporting an orphan and the harness fails, prompting promotion to
// `true_positives/`.
#[cfg(all(unix, windows))]
mod never_built;

pub fn go() -> u32 {
    1
}
