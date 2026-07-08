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

use wl_lint_api::config::{PerCrate, glob_set};

use wl_engine::coverage::CfgShadow;
use wl_engine::fast::FastModel;
use wl_engine::semantic::{
    DeletionUnmask, ExcisionBlock, PrivateOrphan, RemovalSet, SemanticModel,
};

use wl_diagnostic::builder::at_line;
use wl_diagnostic::{Applicability, Diagnostic};
use wl_lint_api::LintId;

use super::config::UnusedPubConfig;
use super::ir::{PubFinding, findings, generated_set};
use wl_lint_api::surgery::deletion::{DeleteOutcome, delete_suggestion};
use wl_lint_api::surgery::import_surgery;

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
    config: &PerCrate<UnusedPubConfig>,
    fast: &FastModel,
    model: &SemanticModel,
    shadow: &CfgShadow,
    suppressed: &dyn Fn(&Diagnostic) -> bool,
) -> CascadeResult {
    // Items an un-editable `use` names (macro-generated, or declared inside a
    // generated file the generator owns): deleting them would dangle an import
    // surgery can't reach, so they never seed a removal.
    let generated = generated_set(fast);
    let blocked = model.import_excision_blocked(&generated);
    // Cfg-shadow veto: identity → the note explaining which uncovered cfg
    // mentions it. Populated lazily as candidates surface (the mention query
    // needs each candidate's name/scope), consulted exactly like `blocked`.
    let mut shadowed: HashMap<String, String> = HashMap::new();
    // Deletion-unmask veto: identity → the `-D warnings` failure its removal
    // would unmask on a SURVIVING item (a field losing its last read, a `len`
    // losing its `is_empty`). Populated per round from the trial removal set;
    // a vetoed item never seeds again (monotone, so the loop still terminates).
    let mut vetoed: HashMap<String, DeletionUnmask> = HashMap::new();
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
        let batch = findings(config, fast, cands, true);
        // Seeds for this round: genuinely-removable (Unused + MachineApplicable
        // deletion) findings that aren't already removed, aren't silenced, and
        // aren't macro-import-blocked.
        let mut newly = Vec::new();
        for f in &batch {
            let Some(id) = &f.id else { continue };
            if !f.removable || removed.contains(id) || blocked.contains_key(id) {
                continue;
            }
            if suppressed(&f.diagnostic) {
                continue; // silenced → won't be applied → callees stay live
            }
            if shadowed.contains_key(id) || vetoed.contains_key(id) {
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
            if removed.contains(&o.id)
                || blocked.contains_key(&o.id)
                || shadowed.contains_key(&o.id)
                || vetoed.contains_key(&o.id)
            {
                continue;
            }
            if let Some(note) = shadow_note(&o.id) {
                shadowed.insert(o.id.clone(), note);
                continue;
            }
            let Some(f) = collateral_finding(&o, config, fast) else {
                continue;
            };
            if f.removable && !suppressed(&f.diagnostic) {
                newly.push(o.id.clone());
            }
        }
        // Deletion-unmask veto: would this round's removals, on top of what's
        // already scheduled, activate a warning on something that SURVIVES?
        // Offenders are pulled out (and stay out); the rest of the round
        // proceeds — the next iteration re-judges with the smaller set.
        if !newly.is_empty() {
            let trial = RemovalSet::new(removed.iter().chain(newly.iter()));
            vetoed.extend(model.deletion_unmasks(&trial, &newly));
            newly.retain(|id| !vetoed.contains_key(id));
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
            let block = f.id.as_deref().and_then(|id| blocked.get(id));
            let shadow_note = f.id.as_deref().and_then(|id| shadowed.get(id));
            let unmask = f.id.as_deref().and_then(|id| vetoed.get(id));
            let transitive = f
                .id
                .as_deref()
                .is_some_and(|id| freed_round.get(id).is_some_and(|&r| r > 0));
            let mut d = f.diagnostic;
            if let Some(block) = block {
                // A blocked item still surfaces as `Unused` with a
                // MachineApplicable delete (findings can't see the import
                // graph) — downgrade it so `--fix` skips the delete that would
                // dangle a `use` surgery can't touch.
                let note = match block {
                    ExcisionBlock::MacroGenerated => {
                        "a `use` of this item is macro-generated and can't be auto-removed; \
                         delete the item and its import by hand"
                    }
                    ExcisionBlock::GeneratedFile => {
                        "a `use` of this item lives in a generated file its generator owns; \
                         fix the generator's inputs, then delete the item"
                    }
                };
                downgrade_deletion(&mut d, note);
            } else if let Some(note) = shadow_note {
                // Possibly used under an uncovered cfg — the veto keeps the
                // item and explains which config would prove it either way.
                downgrade_deletion(&mut d, note);
            } else if let Some(unmask) = unmask {
                // Deleting it would activate a warning on a SURVIVOR — the
                // fixed tree would fail a `-D warnings` gate. Keep it, say
                // exactly what would fire and where.
                downgrade_deletion(&mut d, &unmask_note(unmask));
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
        if blocked.contains_key(&o.id) {
            continue;
        }
        if let Some(f) = collateral_finding(&o, config, fast) {
            let mut d = f.diagnostic;
            if let Some(unmask) = vetoed.get(&o.id) {
                downgrade_deletion(&mut d, &unmask_note(unmask));
            }
            diagnostics.push(d);
        }
    }

    let dangling = model.dangling_imports(&removal, &generated);
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
    per_crate: &PerCrate<UnusedPubConfig>,
    fast: &FastModel,
) -> Option<PubFinding> {
    let krate = fast.members().iter().find(|k| k.code_name() == o.krate)?;
    let config = per_crate.for_crate(&krate.name);
    if config
        .exclude_crates
        .iter()
        .any(|c| c == &krate.name || c == &o.krate)
        || glob_set(&config.allowlist).is_some_and(|al| al.is_match(&o.id))
    {
        return None;
    }
    let span = o.span.as_ref()?;
    let full = o.full_span.as_ref()?;
    let file = fast.root().join(&span.file);
    if file.starts_with(fast.target_directory())
        || glob_set(&config.exclude_paths)
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
/// The veto note for a [`DeletionUnmask`] — names the exact warning the
/// deletion would activate on a survivor, in the style of the narrow-guard
/// notes (`ir.rs`): what fires, on what, and what the human can do instead.
fn unmask_note(unmask: &DeletionUnmask) -> String {
    match unmask {
        DeletionUnmask::UnreadField { owner, field } => format!(
            "deleting this would leave field `{field}` of surviving `{owner}` never-read, \
             tripping `dead_code` on the fixed tree — remove the field first or delete by hand"
        ),
        DeletionUnmask::LenWithoutIsEmpty { owner } => format!(
            "deleting `is_empty` would trip clippy `len_without_is_empty` on `{owner}`'s \
             surviving `len` — remove or keep the pair together"
        ),
    }
}

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
