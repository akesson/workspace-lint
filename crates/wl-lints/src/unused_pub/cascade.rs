//! The one-pass `--fix-auto-delete` cascade: deleting a dead `pub` item frees
//! whatever it solely reached, so this drives `pub_candidates_excluding` to a
//! fixpoint and converges the whole dead chain in a single run (instead of one
//! layer per invocation, each gated behind a commit).
//!
//! It is the deletion path for unused-pub — running it at all *is* the
//! deletion opt-in (the `--fix-auto-delete` flag; plain `--fix` never
//! deletes). Its output *replaces* the plain `ir::check` diagnostics, so it
//! also owns the two safety properties a plain per-item delete can't
//! guarantee alone —
//!
//!  - **Dangling-import cleanup.** A deleted item's `use` sites are excised in
//!    the same fix (`import_surgery`), closing the E0432 hole.
//!  - **Suppression / macro guards.** An `expect!`/`allow!`-silenced item is
//!    never removed (so its callees stay live), and an item some macro-expanded
//!    `use` names — which no edit can excise — is never deleted either.
//!  - **Cfg-shadow veto.** Deletion needs a higher standard of proof than
//!    reporting: an item plausibly mentioned inside a `#[cfg(...)]` region no
//!    declared engine config compiles is *possibly used* on a target the
//!    engine never saw — it is never deleted, and the diagnostic says which
//!    cfg to cover.
//!
//! Removal is monotone (dropping edges only ever *reduces* in-degree), so the
//! loop terminates, bounded by the candidate count.

use std::collections::{HashMap, HashSet};

use wl_engine::coverage::CfgShadow;
use wl_engine::fast::FastModel;
use wl_engine::semantic::{PrivateOrphan, RemovalSet, SemanticModel};

use crate::LintId;
use wl_diagnostic::builder::at_line;
use wl_diagnostic::{Applicability, Diagnostic};

use super::config::UnusedPubConfig;
use super::deletion::{DeleteOutcome, delete_suggestion};
use super::ir::{PubFinding, build_glob_set, findings};
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
    shadow: &CfgShadow,
    suppressed: &dyn Fn(&Diagnostic) -> bool,
) -> CascadeResult {
    // Items an un-editable `use` names (macro-generated): deleting them would
    // dangle an import surgery can't reach, so they never seed a removal.
    let blocked = model.import_excision_blocked();
    // Cfg-shadow veto: identity → the note explaining which uncovered cfg
    // mentions it. Populated lazily as candidates surface (the mention query
    // needs each candidate's name/scope), consulted exactly like `blocked`.
    let mut shadowed: HashMap<String, String> = HashMap::new();
    let shadow_note = |id: &str| -> Option<String> {
        let region = shadow.mention_id(id)?;
        Some(format!(
            "mentioned under `cfg({})` ({}), which no declared `[engine]` config compiles — \
             possibly used on a target the engine never saw; not deleting. Add a matching \
             command to `[engine] configs`, or remove manually",
            region.predicate, region.file,
        ))
    };

    // The cascade round in which each removed item was *first* freed: round 0 is
    // directly-unused (layer-1), round >0 is transitively freed by an earlier
    // round's removals — the distinction the "transitively unused" note renders.
    let mut removed: HashSet<String> = HashSet::new();
    let mut freed_round: HashMap<String, usize> = HashMap::new();
    let mut round = 0usize;
    let final_findings = loop {
        let removal = RemovalSet::new(removed.iter());
        let cands = model.pub_candidates_excluding(&removal);
        let batch = findings(global, per_crate, fast, cands, true);
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
            if shadowed.contains_key(id) {
                continue;
            }
            if let Some(note) = shadow_note(id) {
                shadowed.insert(id.clone(), note);
                continue; // possibly used under an uncovered cfg — never seed
            }
            newly.push(id.clone());
        }
        // Private collateral: defs the removals so far strand (rustc
        // `dead_code` would flag them on the fixed tree). They seed further
        // removal under exactly the pub rules — the deletion must actually be
        // applicable — so a freed private helper's own callees cascade too.
        for o in model.private_orphans(&removal) {
            if removed.contains(&o.id) || blocked.contains(&o.id) || shadowed.contains_key(&o.id) {
                continue;
            }
            if let Some(note) = shadow_note(&o.id) {
                shadowed.insert(o.id.clone(), note);
                continue;
            }
            let Some(f) = collateral_finding(&o, global, per_crate, fast) else {
                continue;
            };
            if f.removable && !suppressed(&f.diagnostic) {
                newly.push(o.id.clone());
            }
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

    let mut diagnostics: Vec<Diagnostic> = final_findings
        .into_iter()
        .map(|f| {
            let is_blocked = f.id.as_deref().is_some_and(|id| blocked.contains(id));
            let shadow_note = f.id.as_deref().and_then(|id| shadowed.get(id));
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
                downgrade_deletion(
                    &mut d,
                    "a `use` of this item is macro-generated and can't be auto-removed; \
                     delete the item and its import by hand",
                );
            } else if let Some(note) = shadow_note {
                // Possibly used under an uncovered cfg — the veto keeps the
                // item and explains which config would prove it either way.
                downgrade_deletion(&mut d, note);
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

    // The converged collateral picture: every private orphan of the final
    // removal set gets its finding (removable ones were seeds; dirty-file
    // ones surface with the skip note, like pub deletions do).
    let removal = RemovalSet::new(removed.iter());
    for o in model.private_orphans(&removal) {
        if blocked.contains(&o.id) {
            continue;
        }
        if let Some(f) = collateral_finding(&o, global, per_crate, fast) {
            diagnostics.push(f.diagnostic);
        }
    }

    let dangling = model.dangling_imports(&removal);
    let surgery = import_surgery(dangling, fast.root());

    CascadeResult {
        diagnostics,
        surgery,
    }
}

/// Build the deletion finding for one private orphan, under the same
/// per-crate config gates as the pub findings (crate / path / allowlist
/// filters, build-generated exclusion, git-clean applicability). `None` ⇒ out
/// of scope: the orphan must not seed a removal and gets no finding.
fn collateral_finding(
    o: &PrivateOrphan,
    global: &UnusedPubConfig,
    per_crate: &HashMap<String, UnusedPubConfig>,
    fast: &FastModel,
) -> Option<PubFinding> {
    let krate = fast.members().iter().find(|k| k.code_name() == o.krate)?;
    let config = per_crate.get(&krate.name).unwrap_or(global);
    if config
        .exclude_crates
        .iter()
        .any(|c| c == &krate.name || c == &o.krate)
        || build_glob_set(&config.allowlist).is_some_and(|al| al.is_match(&o.id))
    {
        return None;
    }
    let span = o.span.as_ref()?;
    let full = o.full_span.as_ref()?;
    let file = fast.root().join(&span.file);
    if file.starts_with(fast.target_directory())
        || build_glob_set(&config.exclude_paths)
            .is_some_and(|ex| ex.is_match(file.to_string_lossy().as_ref()))
    {
        return None;
    }
    let (suggestion, skip_note, removable) = match delete_suggestion(&file, full) {
        DeleteOutcome::Apply(s) => (s, None, true),
        DeleteOutcome::Skip(s, reason) => (s, Some(reason), false),
        DeleteOutcome::Unavailable => return None,
    };
    let builder = at_line(
        LintId::UnusedPub.id(),
        format!(
            "private {} `{}` in crate `{}` loses its last user in this `--fix`",
            o.kind, o.name, o.krate
        ),
        file,
        span.line,
    )
    .help("deleting it too — rustc `dead_code` would flag it on the fixed tree")
    .note("transitively dead: the only item(s) that referenced it are also deleted by this `--fix`")
    .suggestion(suggestion);
    let builder = skip_note.into_iter().fold(builder, |b, r| b.note(r));
    Some(PubFinding {
        id: Some(o.id.clone()),
        removable,
        diagnostic: builder.build(),
    })
}

/// Turn a MachineApplicable item deletion into a `MaybeIncorrect` one so
/// `--fix` leaves it, and explain why.
fn downgrade_deletion(d: &mut Diagnostic, note: &str) {
    let mut changed = false;
    for s in &mut d.suggestions {
        if s.applicability == Applicability::MachineApplicable && s.replacement.is_empty() {
            s.applicability = Applicability::MaybeIncorrect;
            changed = true;
        }
    }
    if changed {
        d.notes.push(note.into());
    }
}
