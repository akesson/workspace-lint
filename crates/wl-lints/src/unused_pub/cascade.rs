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
use std::path::PathBuf;

use wl_lint_api::config::PerCrate;

use wl_engine::coverage::CfgShadow;
use wl_engine::fast::FastModel;
use wl_engine::semantic::{
    BlockReason, DeletionUnmask, ExcisionBlock, PrivateOrphan, RemovalSet, ScaffoldVerdict,
    SemanticModel, TestBlocker, TestScaffold,
};

use wl_diagnostic::Diagnostic;
use wl_diagnostic::builder::at_line;
use wl_lint_api::LintId;

use super::config::UnusedPubConfig;
use super::ir::{PubFinding, findings, generated_set};
use super::scope::{FindingScope, shadow_veto_note};
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
    let shadow_note = |id: &str| -> Option<String> { shadow.mention_id(id).map(shadow_veto_note) };

    // TestOnly-deletion bookkeeping. `test_blocked` (target → veto note) is
    // per-round NON-sticky, unlike `shadowed`/`vetoed`: a blocker referencing
    // two targets may clear once the other target's referrers die in a later
    // round, and re-evaluation is monotone-safe (growing `removed` only ever
    // improves exclusivity). The converged map is what rendering reads.
    // `scaffold_diags` stashes the test-item deletion findings at seed time
    // (the decision is made exactly once); `cleared_note` decorates a deleted
    // target with why its test referrers went with it.
    let mut test_blocked: HashMap<String, String> = HashMap::new();
    let mut scaffold_diags: Vec<PubFinding> = Vec::new();
    let mut cleared_note: HashMap<String, String> = HashMap::new();

    // The cascade round in which each removed item was *first* freed: round 0 is
    // directly-unused (layer-1), round >0 is transitively freed by an earlier
    // round's removals — the distinction the "transitively unused" note renders.
    let mut removed: HashSet<String> = HashSet::new();
    let mut freed_round: HashMap<String, usize> = HashMap::new();
    let mut round = 0usize;
    let final_findings = loop {
        let removal = RemovalSet::new(removed.iter());
        let cands = model.pub_candidates_excluding(&removal);
        let batch = findings(config, fast, cands, true, &generated);
        // Seeds for this round: genuinely-removable (Unused + MachineApplicable
        // deletion) findings that aren't already removed, aren't silenced, and
        // aren't macro-import-blocked. TestOnly findings are DIVERTED — even
        // removable, they may only seed together with their exclusive test
        // scaffolding (the `test_scaffolding` gate below).
        let mut newly = Vec::new();
        let mut pending_test_only: Vec<String> = Vec::new();
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
            if f.test_only {
                pending_test_only.push(id.clone());
                continue;
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
            let Some(f) = collateral_finding(&o, config, fast, &generated) else {
                continue;
            };
            if f.removable && !suppressed(&f.diagnostic) {
                newly.push(o.id.clone());
            }
        }
        // TestOnly targets: deletable only together with their exclusive test
        // scaffolding (see `gate_test_only`). Blockers land in `test_blocked`
        // (recomputed each round); cleared groups join this round's trial.
        test_blocked.clear();
        let mut groups = gate_test_only(
            &GateCtx {
                model,
                config,
                fast,
                generated: &generated,
                removed: &removed,
                blocked: &blocked,
                vetoed: &vetoed,
                shadow_note: &shadow_note,
                suppressed,
            },
            &removal,
            &pending_test_only,
            &mut test_blocked,
        );
        // Deletion-unmask veto: would this round's removals, on top of what's
        // already scheduled, activate a warning on something that SURVIVES?
        // Offenders are pulled out (and stay out); the rest of the round
        // proceeds — the next iteration re-judges with the smaller set.
        // Scaffolding groups ride the same trial ATOMICALLY: a hit on any
        // member pulls the whole group (deleting the target without a test,
        // or a test without its target, breaks the build).
        if !newly.is_empty() || !groups.is_empty() {
            let group_ids: Vec<String> = groups
                .iter()
                .flat_map(|(t, fs)| {
                    std::iter::once(t.clone()).chain(fs.iter().filter_map(|f| f.id.clone()))
                })
                .collect();
            let trial_new: Vec<String> = newly.iter().chain(group_ids.iter()).cloned().collect();
            let trial = RemovalSet::new(removed.iter().chain(trial_new.iter()));
            vetoed.extend(model.deletion_unmasks(&trial, &trial_new));
            newly.retain(|id| !vetoed.contains_key(id));
            drop_unmasked_groups(&mut groups, &vetoed, &mut test_blocked);
            commit_groups(groups, &mut newly, &mut cleared_note, &mut scaffold_diags);
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
                d.withhold_deletions(note);
            } else if let Some(note) = shadow_note {
                // Possibly used under an uncovered cfg — the veto keeps the
                // item and explains which config would prove it either way.
                d.withhold_deletions(note);
            } else if let Some(unmask) = unmask {
                // Deleting it would activate a warning on a SURVIVOR — the
                // fixed tree would fail a `-D warnings` gate. Keep it, say
                // exactly what would fire and where.
                d.withhold_deletions(&unmask_note(unmask));
            } else if let Some(note) = f.id.as_deref().and_then(|id| test_blocked.get(id)) {
                // A TestOnly target whose referencing tests aren't exclusive
                // scaffolding — the deletion stays shown-not-applied, and the
                // note names the blocking test.
                d.withhold_deletions(note);
            } else if let Some(note) = f.id.as_deref().and_then(|id| cleared_note.get(id)) {
                // A deleted TestOnly target — say why the test items went too.
                d.notes.push(note.clone().into());
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
        if let Some(f) = collateral_finding(&o, config, fast, &generated) {
            let mut d = f.diagnostic;
            if let Some(unmask) = vetoed.get(&o.id) {
                d.withhold_deletions(&unmask_note(unmask));
            }
            diagnostics.push(d);
        }
    }

    // The test-scaffolding deletions, stashed at seed time (each decision is
    // made exactly once; the items are in `removed`, so the import surgery
    // below already trims their `use`s).
    diagnostics.extend(scaffold_diags.into_iter().map(|f| f.diagnostic));

    let dangling = model.dangling_imports(&removal, &generated);
    let surgery = import_surgery(dangling, fast.root());

    CascadeResult {
        diagnostics,
        surgery,
    }
}

/// Everything the per-round TestOnly gate consults — bundled so the gate and
/// its per-scaffold checks share one signature.
struct GateCtx<'a> {
    model: &'a SemanticModel,
    config: &'a PerCrate<UnusedPubConfig>,
    fast: &'a FastModel,
    generated: &'a HashSet<PathBuf>,
    removed: &'a HashSet<String>,
    blocked: &'a HashMap<String, ExcisionBlock>,
    vetoed: &'a HashMap<String, DeletionUnmask>,
    shadow_note: &'a dyn Fn(&str) -> Option<String>,
    suppressed: &'a dyn Fn(&Diagnostic) -> bool,
}

/// The per-round TestOnly gate: the engine partitions each pending target's
/// referencing test items into a deletable closure or a blocker; the lint
/// layer then applies its own gates to every scaffold — and the veto is
/// INVERTED relative to private collateral: a private orphan left behind is
/// a warning, a test referencing a deleted item is a broken build, so if ANY
/// scaffold of a target can't be deleted the TARGET stays (its veto note
/// lands in `test_blocked`). Cleared targets return with their scaffold
/// findings, ready for the unmask trial.
fn gate_test_only(
    cx: &GateCtx<'_>,
    removal: &RemovalSet,
    pending: &[String],
    test_blocked: &mut HashMap<String, String>,
) -> Vec<(String, Vec<PubFinding>)> {
    let mut groups = Vec::new();
    if pending.is_empty() {
        return groups;
    }
    for (target, verdict) in cx.model.test_scaffolding(removal, pending).per_target {
        match verdict {
            ScaffoldVerdict::Blocked(b) => {
                test_blocked.insert(target, test_block_note(&b));
            }
            ScaffoldVerdict::Cleared { scaffolding } => match gate_scaffolding(cx, &scaffolding) {
                Ok(sfindings) => groups.push((target, sfindings)),
                Err(note) => {
                    test_blocked.insert(target, note);
                }
            },
        }
    }
    groups
}

/// Apply the lint-layer gates to one cleared closure: every scaffold must be
/// genuinely deletable (git-clean, unsilenced, in scope, not macro-import-
/// blocked, not cfg-shadowed) or the whole group fails with a veto note.
fn gate_scaffolding(
    cx: &GateCtx<'_>,
    scaffolding: &[TestScaffold],
) -> Result<Vec<PubFinding>, String> {
    let mut sfindings = Vec::new();
    for s in scaffolding {
        if cx.removed.contains(&s.id) {
            continue; // already scheduled by a sibling target
        }
        if cx.blocked.contains_key(&s.id)
            || cx.vetoed.contains_key(&s.id)
            || (cx.shadow_note)(&s.id).is_some()
        {
            return Err(scaffold_veto_note(s));
        }
        match scaffold_finding(s, cx.config, cx.fast, cx.generated) {
            Some(f) if f.removable && !(cx.suppressed)(&f.diagnostic) => sfindings.push(f),
            _ => return Err(scaffold_veto_note(s)),
        }
    }
    Ok(sfindings)
}

/// Pull every group the unmask trial hit — ATOMICALLY (deleting the target
/// without a test, or a test without its target, breaks the build) — and
/// record the veto note on the target.
fn drop_unmasked_groups(
    groups: &mut Vec<(String, Vec<PubFinding>)>,
    vetoed: &HashMap<String, DeletionUnmask>,
    test_blocked: &mut HashMap<String, String>,
) {
    groups.retain(|(target, fs)| {
        let hit = vetoed.contains_key(target)
            || fs
                .iter()
                .any(|f| f.id.as_deref().is_some_and(|id| vetoed.contains_key(id)));
        if hit {
            test_blocked.insert(
                target.clone(),
                "deleting it together with its tests would unmask a `-D warnings` \
                 failure on surviving code — resolve that first or delete by hand"
                    .into(),
            );
        }
        !hit
    });
}

/// Schedule the surviving groups: target + scaffolds seed the removal set,
/// the scaffold findings are stashed for final rendering, and the target
/// gets its "deleted together with its tests" note. A test fn exercising two
/// targets clears under BOTH — commit its finding once, not per target.
fn commit_groups(
    groups: Vec<(String, Vec<PubFinding>)>,
    newly: &mut Vec<String>,
    cleared_note: &mut HashMap<String, String>,
    scaffold_diags: &mut Vec<PubFinding>,
) {
    for (target, fs) in groups {
        cleared_note.insert(
            target.clone(),
            format!(
                "its only referrer(s) were test code — {} exclusively-scaffolding \
                 test item(s) are deleted alongside it by this `--fix`",
                fs.len()
            ),
        );
        newly.push(target);
        for f in fs {
            let Some(id) = &f.id else { continue };
            if newly.contains(id) {
                continue; // shared scaffold, committed by a sibling target
            }
            newly.push(id.clone());
            scaffold_diags.push(f);
        }
    }
}

/// The veto note for a [`TestBlocker`] — names the blocking test item (with
/// its `file:line`) and what anchors it to surviving code.
fn test_block_note(b: &TestBlocker) -> String {
    let anchor = b
        .span
        .as_ref()
        .map(|s| format!(" ({}:{})", s.file, s.line))
        .unwrap_or_default();
    match &b.reason {
        BlockReason::ReachesSurviving { to } => format!(
            "only test code references it, but test item `{}`{anchor} also exercises \
             surviving `{to}` — deleting would orphan that test; update or remove the test \
             first, or delete both by hand",
            b.test
        ),
        BlockReason::KeptBySurvivor { from } => format!(
            "only test code references it, but its test referrer `{}`{anchor} is still used \
             by surviving `{from}` (a shared fixture/helper) — untangle them or delete by hand",
            b.test
        ),
        BlockReason::NotDeletable => format!(
            "only test code references it, and test item `{}`{anchor} has no safe \
             auto-delete surface — delete the item and its tests by hand",
            b.test
        ),
    }
}

/// The veto note when a scaffold clears the ENGINE's exclusivity proof but
/// fails a lint-layer gate (suppressed, dirty file, excluded path, cfg-shadow,
/// macro-import-blocked): the target must stay — deleting it without the test
/// would break the test build.
fn scaffold_veto_note(s: &TestScaffold) -> String {
    let anchor = s
        .span
        .as_ref()
        .map(|sp| format!(" ({}:{})", sp.file, sp.line))
        .unwrap_or_default();
    format!(
        "only test code references it, and referencing test item `{}`{anchor} can't be \
         auto-deleted with it (silenced, git-dirty, or out of fix scope) — resolve the item \
         and its tests by hand",
        s.id
    )
}

/// Build the deletion finding for one exclusive test scaffold, under the same
/// per-crate config gates as the collateral findings. The owning member is
/// resolved by crate code-name first, then by span-file prefix — an
/// integration-test target crate's code name (the test file stem) is not a
/// member name, but its sources live under the member's directory.
fn scaffold_finding(
    s: &TestScaffold,
    per_crate: &PerCrate<UnusedPubConfig>,
    fast: &FastModel,
    generated: &HashSet<PathBuf>,
) -> Option<PubFinding> {
    let span = s.span.as_ref()?;
    let full = s.full_span.as_ref()?;
    let file = fast.root().join(&span.file);
    let krate = fast
        .members()
        .iter()
        .find(|k| k.code_name() == s.krate)
        .or_else(|| {
            fast.members()
                .iter()
                .find(|k| file.starts_with(&k.manifest_dir))
        })?;
    let config = per_crate.for_crate(&krate.name);
    let scope = FindingScope::new(config, fast, krate, generated, None);
    if scope.crate_excluded(&s.krate) || scope.skips(&s.id, &s.kind, Some(span)) {
        return None;
    }
    let (suggestion, skip_note, removable) = match delete_suggestion(&file, full) {
        DeleteOutcome::Apply(sg) => (sg, None, true),
        DeleteOutcome::Skip(sg, reason) => (sg, Some(reason), false),
        DeleteOutcome::Unavailable => return None,
    };
    let builder = at_line(
        LintId::UnusedPub.id(),
        format!(
            "test {} `{}` in crate `{}` only exercises items deleted by this `--fix`",
            s.kind, s.name, s.krate
        ),
        file,
        span.line,
    )
    .help("deleting it too — it would reference deleted items and break the test build")
    .note(
        "exclusive test scaffolding: every workspace item it references is also deleted by this `--fix`",
    )
    .suggestion(suggestion);
    let builder = skip_note.into_iter().fold(builder, |b, r| b.note(r));
    Some(PubFinding {
        id: Some(s.id.clone()),
        removable,
        test_only: false,
        diagnostic: builder.build(),
    })
}

/// Build the deletion finding for one private orphan, under the same
/// per-crate config gates as the pub findings (crate / path / allowlist
/// filters, build-generated exclusion, git-clean applicability). `None` ⇒ out
/// of scope: the orphan must not seed a removal and gets no finding.
fn collateral_finding(
    o: &PrivateOrphan,
    per_crate: &PerCrate<UnusedPubConfig>,
    fast: &FastModel,
    generated: &HashSet<PathBuf>,
) -> Option<PubFinding> {
    let krate = fast.members().iter().find(|k| k.code_name() == o.krate)?;
    let config = per_crate.for_crate(&krate.name);
    let span = o.span.as_ref()?;
    let full = o.full_span.as_ref()?;
    let file = fast.root().join(&span.file);
    let scope = FindingScope::new(config, fast, krate, generated, None);
    if scope.crate_excluded(&o.krate) || scope.skips(&o.id, &o.kind, Some(span)) {
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
        test_only: false,
        diagnostic: builder.build(),
    })
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use wl_engine::wl_ir::Span;

    fn finding(id: &str) -> PubFinding {
        PubFinding {
            id: Some(id.into()),
            removable: true,
            test_only: false,
            diagnostic: at_line(LintId::UnusedPub.id(), "x", "src/lib.rs", 1).build(),
        }
    }

    fn span() -> Option<Span> {
        Some(Span {
            file: "src/lib.rs".into(),
            lo: 0,
            hi: 10,
            line: 214,
            from_expansion: false,
        })
    }

    fn unmask() -> DeletionUnmask {
        DeletionUnmask::UnreadField {
            owner: "a::S".into(),
            field: "f".into(),
        }
    }

    /// A test fn exercising two targets clears under both — its finding (and
    /// removal seed) must be committed once, while each target keeps its own
    /// cleared note.
    #[test]
    fn commit_groups_commits_a_shared_scaffold_once() {
        let groups = vec![
            ("a::x".to_string(), vec![finding("b::tests::t")]),
            ("a::y".to_string(), vec![finding("b::tests::t")]),
        ];
        let mut newly = Vec::new();
        let mut notes = HashMap::new();
        let mut diags = Vec::new();
        commit_groups(groups, &mut newly, &mut notes, &mut diags);
        assert_eq!(newly, ["a::x", "b::tests::t", "a::y"]);
        assert_eq!(
            diags.len(),
            1,
            "one scaffold diagnostic, not one per target"
        );
        assert!(notes.contains_key("a::x") && notes.contains_key("a::y"));
    }

    /// The unmask trial pulls a group whether the hit landed on the target or
    /// on one of its scaffolds — never half a group — and vetoes the target.
    #[test]
    fn drop_unmasked_groups_pulls_whole_group_atomically() {
        let mut groups = vec![
            ("a::x".to_string(), vec![finding("b::tests::t1")]),
            ("a::y".to_string(), vec![finding("b::tests::t2")]),
            ("a::z".to_string(), vec![finding("b::tests::t3")]),
        ];
        let vetoed = HashMap::from([
            ("b::tests::t1".to_string(), unmask()),
            ("a::y".to_string(), unmask()),
        ]);
        let mut test_blocked = HashMap::new();
        drop_unmasked_groups(&mut groups, &vetoed, &mut test_blocked);
        let survivors: Vec<&str> = groups.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(survivors, ["a::z"]);
        assert!(
            test_blocked.contains_key("a::x"),
            "scaffold hit vetoes the target"
        );
        assert!(
            test_blocked.contains_key("a::y"),
            "target hit vetoes the target"
        );
        assert!(!test_blocked.contains_key("a::z"));
    }

    /// Each blocker reason renders its own actionable note, anchored at the
    /// blocking test's `file:line` when the span is known.
    #[test]
    fn test_block_note_names_each_reason() {
        let blocker = |reason| TestBlocker {
            test: "b::tests::t".into(),
            span: span(),
            reason,
        };
        let reaches = test_block_note(&blocker(BlockReason::ReachesSurviving {
            to: "a::kept".into(),
        }));
        assert!(
            reaches.contains("`b::tests::t` (src/lib.rs:214)"),
            "{reaches}"
        );
        assert!(
            reaches.contains("exercises surviving `a::kept`"),
            "{reaches}"
        );
        let kept = test_block_note(&blocker(BlockReason::KeptBySurvivor {
            from: "b::tests::fixture".into(),
        }));
        assert!(kept.contains("still used"), "{kept}");
        assert!(kept.contains("`b::tests::fixture`"), "{kept}");
        let not_deletable = test_block_note(&blocker(BlockReason::NotDeletable));
        assert!(not_deletable.contains("no safe"), "{not_deletable}");
        // Span unknown → no dangling anchor parenthesis.
        let spanless = test_block_note(&TestBlocker {
            test: "b::tests::t".into(),
            span: None,
            reason: BlockReason::NotDeletable,
        });
        assert!(spanless.contains("`b::tests::t` has"), "{spanless}");
    }

    /// The lint-layer veto (engine cleared, scaffold out of fix scope) names
    /// the test item the same way.
    #[test]
    fn scaffold_veto_note_names_the_test_item() {
        let note = scaffold_veto_note(&TestScaffold {
            id: "b::tests::t".into(),
            krate: "b".into(),
            name: "t".into(),
            kind: "fn".into(),
            span: span(),
            full_span: span(),
        });
        assert!(note.contains("`b::tests::t` (src/lib.rs:214)"), "{note}");
        assert!(note.contains("can't be auto-deleted"), "{note}");
    }
}
