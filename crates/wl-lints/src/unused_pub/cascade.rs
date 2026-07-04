//! The one-pass `--fix` cascade: deleting a dead `pub` item frees whatever it
//! solely reached, so this drives `pub_candidates_excluding` to a fixpoint and
//! converges the whole dead chain in a single run (instead of one layer per
//! invocation, each gated behind a commit).
//!
//! It is the `--fix` path for unused-pub: its output *replaces* the plain
//! `ir::check` diagnostics, so it also owns the two safety
//! properties a plain per-item delete can't guarantee alone —
//!
//!  - **Dangling-import cleanup.** A deleted item's `use` sites are excised in
//!    the same fix (`import_surgery`), closing the E0432 hole.
//!  - **Suppression / macro guards.** An `expect!`/`allow!`-silenced item is
//!    never removed (so its callees stay live), and an item some macro-expanded
//!    `use` names — which no edit can excise — is never deleted either.
//!
//! Removal is monotone (dropping edges only ever *reduces* in-degree), so the
//! loop terminates, bounded by the candidate count.

use std::collections::{HashMap, HashSet};

use wl_engine::fast::FastModel;
use wl_engine::semantic::{RemovalSet, SemanticModel};

use wl_diagnostic::{Applicability, Diagnostic};

use super::config::UnusedPubConfig;
use super::ir::findings;
use super::surgery::import_surgery;

pub struct CascadeResult {
    /// The converged unused-pub diagnostics — replace the plain-check set.
    pub diagnostics: Vec<Diagnostic>,
    /// Import-deletion diagnostics for the `use`s the removals left dangling.
    pub surgery: Vec<Diagnostic>,
}

/// Run the cascade. `suppressed` is a **non-mutating** query against the
/// directive map (an `expect!`/`allow!`-silenced deletion won't be applied, so
/// its target must not seed a removal); the main suppression pass runs later
/// and does the real filtering + stale-expect accounting over this output.
pub fn run(
    global: &UnusedPubConfig,
    per_crate: &HashMap<String, UnusedPubConfig>,
    fast: &FastModel,
    model: &SemanticModel,
    suppressed: &dyn Fn(&Diagnostic) -> bool,
) -> CascadeResult {
    // Items an un-editable `use` names (macro-generated): deleting them would
    // dangle an import surgery can't reach, so they never seed a removal.
    let blocked = model.import_excision_blocked();

    // The cascade round in which each removed item was *first* freed: round 0 is
    // directly-unused (layer-1), round >0 is transitively freed by an earlier
    // round's removals — the distinction the "transitively unused" note renders.
    let mut removed: HashSet<String> = HashSet::new();
    let mut freed_round: HashMap<String, usize> = HashMap::new();
    let mut round = 0usize;
    let final_findings = loop {
        let cands = model.pub_candidates_excluding(&RemovalSet::new(removed.iter()));
        let batch = findings(global, per_crate, fast, cands);
        // Seeds for this round: genuinely-removable (Unused + MachineApplicable
        // deletion) findings that aren't already removed, aren't silenced, and
        // aren't macro-import-blocked.
        let mut newly = Vec::new();
        for f in &batch {
            let Some(id) = &f.id else { continue };
            if !f.removable || removed.contains(id) || blocked.contains(id) {
                continue;
            }
            if suppressed(&f.diagnostic) {
                continue; // silenced → won't be applied → callees stay live
            }
            newly.push(id.clone());
        }
        if newly.is_empty() {
            break batch; // converged; this batch is the final picture
        }
        for id in &newly {
            freed_round.insert(id.clone(), round);
        }
        removed.extend(newly);
        round += 1;
    };

    let diagnostics = final_findings
        .into_iter()
        .map(|f| {
            let is_blocked = f.id.as_deref().is_some_and(|id| blocked.contains(id));
            let transitive = f
                .id
                .as_deref()
                .is_some_and(|id| freed_round.get(id).is_some_and(|&r| r > 0));
            let mut d = f.diagnostic;
            if is_blocked {
                // A blocked item still surfaces as `Unused` with a
                // MachineApplicable delete (findings can't see the import
                // graph) — downgrade it so `--fix` skips the delete that would
                // dangle a macro-generated `use`.
                downgrade_deletion(&mut d);
            } else if transitive {
                // Not obviously dead in the source (something *does* reference
                // it) — it only becomes unused because that referrer is deleted
                // in this same pass. Say so, so the removal doesn't read as a
                // mistake.
                d.notes.push(
                    "transitively unused: the only item(s) that referenced it are also deleted by this `--fix`"
                        .into(),
                );
            }
            d
        })
        .collect();

    let dangling = model.dangling_imports(&RemovalSet::new(removed.iter()));
    let surgery = import_surgery(dangling, fast.root());

    CascadeResult {
        diagnostics,
        surgery,
    }
}

/// Turn a MachineApplicable item deletion into a `MaybeIncorrect` one so
/// `--fix` leaves it, and explain why.
fn downgrade_deletion(d: &mut Diagnostic) {
    let mut changed = false;
    for s in &mut d.suggestions {
        if s.applicability == Applicability::MachineApplicable && s.replacement.is_empty() {
            s.applicability = Applicability::MaybeIncorrect;
            changed = true;
        }
    }
    if changed {
        d.notes.push(
            "a `use` of this item is macro-generated and can't be auto-removed; \
             delete the item and its import by hand"
                .into(),
        );
    }
}
