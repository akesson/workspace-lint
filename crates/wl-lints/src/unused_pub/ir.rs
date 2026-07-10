//! The rustc-IR `unused-pub` backend (the default).
//!
//! Each syn query maps onto an engine primitive:
//!
//! - `Workspace::referring_crates` / `referenced_from_sibling_target` →
//!   [`PubUsage`]: the cfg-matrix-unioned cross/intra/unused split. Sibling
//!   targets (integration tests, benches) compile as their own crates, so
//!   their references classify cross-crate exactly like the syn rule — but
//!   only under a config that compiles them (`[engine] configs`).
//! - `Workspace::re_exports().is_target` → [`PubCandidate::reexport_target`].
//! - `Workspace::exposed_in_public_signature` →
//!   [`PubCandidate::signature_exposed`] (from lowered signatures, so the
//!   builder-macro exemptions syn special-cased come for free).
//! - `Workspace::is_externally_reachable` →
//!   [`PubCandidate::externally_reachable`] (pub-module-hop over `mod` facts).
//! - `Workspace::macro_implicit_refs_for` → **gone**: rustc sees macro
//!   expansions natively, so a macro-generated reference is an ordinary edge
//!   and the `expansion-uses` annotations are obsolete.
//! - `Workspace::resolved_publish` → [`FastModel::resolved_publish`].
//!
//! What the engine adds over syn: inherent-impl methods are candidates (the
//! `pub_method_in_impl_block` false negative flips to a true positive), and
//! trait-dispatch / `#[no_mangle]`-export reachability retires the
//! `ffi_no_mangle_export` false positive. Trait-impl items are judged for
//! reachability but never flagged — their visibility is trait-forced (no
//! tighten surface, deletion breaks the impl), mirroring rustc's own
//! `dead_code` scope.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use wl_engine::fast::{CrateInfo, FastModel, Publish};
use wl_engine::semantic::{Category, PubCandidate, PubUsage, SemanticModel};
use wl_engine::wl_ir;

use wl_diagnostic::builder::{at_crate, at_line};
use wl_diagnostic::{Applicability, Diagnostic, PubVerdict};
use wl_lint_api::LintId;
use wl_lint_api::config::PerCrate;

use wl_engine::coverage::CfgShadow;

use super::DEFAULT_PUBLISH_HINT_THRESHOLD;
use super::config::UnusedPubConfig;
use super::scope::{FindingScope, shadow_narrow_note, shadow_report_note};
use wl_lint_api::surgery::deletion::pick_deletion_fix;

/// One unused-pub finding paired with the two things the `--fix-auto-delete`
/// cascade needs beyond the rendered diagnostic: the candidate identity (the
/// removed-set key) and whether it is a genuinely-`Unused` item carrying a
/// MachineApplicable deletion — i.e. a removal seed that will actually be
/// applied.
pub(crate) struct PubFinding {
    /// `PubCandidate::id`; `None` for the crate-level publish hint.
    pub id: Option<String>,
    /// `Unused` + deletion mode + git-clean ⇒ this item's deletion is
    /// MachineApplicable, so removing it (and cascading) is sound.
    pub removable: bool,
    /// `TestOnly` verdict: even when `removable`, the cascade must NOT seed
    /// this directly — deletion is sound only together with its exclusively-
    /// scaffolding tests (the `test_scaffolding` gate), never alone.
    pub test_only: bool,
    pub diagnostic: Diagnostic,
}

pub(super) fn check(
    config: &PerCrate<UnusedPubConfig>,
    fast: &FastModel,
    model: &SemanticModel,
    shadow: Option<&CfgShadow>,
) -> Vec<Diagnostic> {
    let generated = generated_set(fast);
    // Deletion is a `--fix-auto-delete` concern, and that path replaces this
    // plain-check output with the cascade's — so the plain run always renders
    // the tighten fallback for `Unused` items, never a deletion.
    findings_with_shadow(
        config,
        fast,
        model.pub_candidates(),
        false,
        &generated,
        shadow,
    )
    .into_iter()
    .map(|f| f.diagnostic)
    .collect()
}

/// The candidate-driven core of the lint — shared by the plain [`check`] (fed
/// `SemanticModel::pub_candidates`) and the `--fix-auto-delete` cascade (fed
/// `pub_candidates_excluding`). Returns per-candidate [`PubFinding`]s plus the
/// crate-level publish hints, in the same order [`check`] always emitted them.
/// `auto_delete` selects the structural fix for `Unused` items: whole-item
/// deletion (the cascade) vs the shown-but-not-applied tighten fallback.
/// `generated` is [`generated_set`], computed once by the caller (the cascade
/// calls this every fixpoint round).
pub(crate) fn findings(
    config: &PerCrate<UnusedPubConfig>,
    fast: &FastModel,
    candidates: Vec<PubCandidate>,
    auto_delete: bool,
    generated: &HashSet<PathBuf>,
) -> Vec<PubFinding> {
    findings_with_shadow(config, fast, candidates, auto_delete, generated, None)
}

/// [`findings`] with the report-time cfg-shadow index: an `Unused` candidate
/// mentioned in a region no config compiles gets the specific
/// "possibly used under `cfg(...)`" note instead of the generic blind-spot
/// one. The cascade passes `None` — its veto handles shadowing itself.
fn findings_with_shadow(
    per_crate: &PerCrate<UnusedPubConfig>,
    fast: &FastModel,
    candidates: Vec<PubCandidate>,
    auto_delete: bool,
    generated: &HashSet<PathBuf>,
    shadow: Option<&CfgShadow>,
) -> Vec<PubFinding> {
    let mut by_crate: BTreeMap<&str, Vec<&PubCandidate>> = BTreeMap::new();
    for c in &candidates {
        by_crate.entry(c.krate.as_str()).or_default().push(c);
    }
    // Within a crate, report in source order (file, then line) — the closest
    // stable analog of the syn module-tree walk order.
    for list in by_crate.values_mut() {
        list.sort_by_key(|c| {
            c.span
                .as_ref()
                .map(|s| (s.file.clone(), s.line))
                .unwrap_or_default()
        });
    }

    let mut out = Vec::new();
    for krate in fast.members() {
        // A per-crate `[crates.<name>.unused-pub]` wholesale-replaces the
        // global params for this crate; the scope's glob sets / kind filter
        // are built from the resolved config, so they're computed per crate.
        let config = per_crate.for_crate(&krate.name);
        let crate_code = krate.code_name();
        let kind_filter: Option<HashSet<&'static str>> = (!config.kinds.is_empty())
            .then(|| config.kinds.iter().map(|k| k.to_ir_kind()).collect());
        let scope = FindingScope::new(config, fast, krate, generated, kind_filter);
        if scope.crate_excluded(&crate_code) {
            continue;
        }
        let Some(cands) = by_crate.get(crate_code.as_str()) else {
            continue;
        };
        // A crate's library-public items are exempt as "external API surface"
        // only when the crate actually has out-of-workspace consumers — i.e.
        // it declares `publish = true` (or a registry list) — or the user
        // opted every crate in via `assume-all-public`.
        let exempt_external_api = config.assume_all_public
            || matches!(
                fast.resolved_publish(krate),
                Publish::ExplicitTrue | Publish::Registries(_)
            );
        let ctx = CheckCtx {
            scope,
            crate_code: &crate_code,
            suppress_intra_crate: config.suppress_intra_crate,
            auto_delete,
            exempt_external_api,
            shadow,
        };
        let mut crate_findings = Vec::new();
        for cand in cands {
            if let Some(f) = check_candidate(cand, &ctx) {
                crate_findings.push(f);
            }
        }
        // When a *workspace-internal* crate accumulates several findings, the
        // likely cause is that it really is published — nudge the author
        // toward the one-line fix. Self-resolving: adding `publish = true`
        // exempts the items, so the findings and this hint both disappear.
        let threshold = config
            .publish_hint_threshold
            .unwrap_or(DEFAULT_PUBLISH_HINT_THRESHOLD);
        if !exempt_external_api && threshold > 0 && crate_findings.len() >= threshold {
            out.push(PubFinding {
                id: None,
                removable: false,
                test_only: false,
                diagnostic: publish_hint(fast, krate, &crate_code, crate_findings.len()),
            });
        }
        out.extend(crate_findings);
    }
    out
}

/// Workspace-relative paths of checked-in generated (`include!`d) files.
/// Candidates there are excluded at the source of every finding path — inside
/// [`findings`] rather than by the binary's stream-level
/// `drop_generated_anchored`, which runs *before* the cascade replaces the
/// unused-pub findings and so can't keep generated findings out of a
/// `--fix-auto-delete` run (nor out of the publish-hint rollup and machine-fix
/// tally, which count pre-drop). Both sides of the membership test share the
/// include-resolver's lexical form (a relative include stays `src/../gen/x.rs`),
/// so exact equality on workspace-relative paths is the right comparison.
pub(crate) fn generated_set(fast: &FastModel) -> HashSet<PathBuf> {
    fast.generated_files()
        .map(|p| fast.crate_relative_path(p))
        .collect()
}

struct CheckCtx<'a> {
    /// The per-crate scope gates (crate/kind/allowlist/paths/generated) —
    /// the ONE implementation shared with the cascade's scaffold/collateral
    /// paths (see `scope.rs`).
    scope: FindingScope<'a>,
    crate_code: &'a str,
    suppress_intra_crate: bool,
    auto_delete: bool,
    /// Whether library-public items in this crate are exempt as external API
    /// surface (the crate is published, or `assume-all-public` is set).
    exempt_external_api: bool,
    /// Report-time cfg-shadow index (`None` in the cascade path).
    shadow: Option<&'a CfgShadow>,
}

impl CheckCtx<'_> {
    fn shadow_mention(&self, id: &str) -> Option<&wl_engine::coverage::ShadowRegion> {
        self.shadow.and_then(|s| s.mention_id(id))
    }
}

fn check_candidate(cand: &PubCandidate, ctx: &CheckCtx<'_>) -> Option<PubFinding> {
    if candidate_skipped_by_filters(cand, ctx) {
        return None;
    }
    let verdict = match cand.usage {
        // Production cross-crate or dispatch/export-reached: provably in use —
        // leave alone. (Dispatch covers the `ffi_no_mangle_export` class syn
        // false-positived on.)
        PubUsage::CrossCrate | PubUsage::DispatchReached => return None,
        PubUsage::IntraCrate if ctx.suppress_intra_crate => return None,
        PubUsage::IntraCrate => PubVerdict::IntraCrate,
        // Reached only from test code: production-dead. `suppress-intra-crate`
        // does NOT silence it — that knob drops tighten advice, and this is a
        // dead-code finding.
        PubUsage::TestOnly => PubVerdict::TestOnly,
        PubUsage::Unused => PubVerdict::Unused,
    };
    let span = cand.span.as_ref()?;
    let (diagnostic, removable) = build_diagnostic(cand, ctx, span, verdict);
    Some(PubFinding {
        id: Some(cand.id.clone()),
        removable,
        test_only: verdict == PubVerdict::TestOnly,
        diagnostic,
    })
}

/// Pure filter cascade — every reason to bail before composing a diagnostic,
/// mirroring the syn backend's `item_skipped_by_filters`. The *scope* gates
/// (kind/allowlist/paths/target-dir/generated) live in [`FindingScope`];
/// only the candidate-*semantic* guards stay here.
fn candidate_skipped_by_filters(cand: &PubCandidate, ctx: &CheckCtx<'_>) -> bool {
    // Findings target module-level and inherent-impl items. A trait-impl
    // item's visibility is trait-forced — no tighten surface, deletion breaks
    // the `impl` — so it is judged (dispatch reachability) but never flagged.
    if !matches!(
        cand.category,
        Category::ModuleLevel | Category::InherentImpl
    ) {
        return true;
    }
    // The crate root `main` of a bin target.
    if cand.name == "main" && cand.id.split("::").count() == 2 {
        return true;
    }
    if ctx.scope.skips(&cand.id, &cand.kind, cand.span.as_ref()) {
        return true;
    }
    // A re-export target is part of the crate's API regardless of publish
    // status — narrowing it would break the `pub use` (E0364 / E0365).
    if cand.reexport_target {
        return true;
    }
    // Named in the *public signature* of a more-visible item: tightening
    // would not compile (E0446 / `private_interfaces`). Publish-independent,
    // so it runs before the `exempt_external_api`-gated reachability guard.
    if cand.signature_exposed {
        return true;
    }
    // Library-public reachability only exempts the item when the crate is
    // treated as having external (out-of-workspace) consumers.
    if ctx.exempt_external_api && cand.externally_reachable {
        return true;
    }
    false
}

/// Build the rendered diagnostic, returning `(diagnostic, removable)` where
/// `removable` is `true` iff the structural fix is a **MachineApplicable
/// deletion** — the signal the cascade seeds its removed set from.
fn build_diagnostic(
    cand: &PubCandidate,
    ctx: &CheckCtx<'_>,
    span: &wl_ir::Span,
    verdict: PubVerdict,
) -> (Diagnostic, bool) {
    let kind_str = &cand.kind;
    let crate_code = ctx.crate_code;
    let (message, suggestion) = match verdict {
        PubVerdict::IntraCrate => (
            format!(
                "pub {kind_str} `{}` in crate `{crate_code}` is only used inside the crate",
                cand.name
            ),
            "consider `pub(crate)` to tighten visibility",
        ),
        PubVerdict::TestOnly => (
            format!(
                "pub {kind_str} `{}` in crate `{crate_code}` is only used by test code",
                cand.name
            ),
            "gate it `#[cfg(test)]`, move it into test code, or remove it",
        ),
        PubVerdict::Unused => (
            format!(
                "pub {kind_str} `{}` in crate `{crate_code}` appears unused — consider removing",
                cand.name
            ),
            "remove the item or its `pub` visibility",
        ),
    };

    // Workspace-relative, like every other lint's anchor (the IR span is
    // already relative, and the whole run is workspace-rooted): an absolute
    // path here leaked the checkout prefix into the rendered findings — the
    // one lint that did (2026-07-10 validation, Issue 9). The same relative
    // path serves the suggestion spans and file reads below (cwd = root).
    let file = PathBuf::from(&span.file);
    let builder =
        at_line(LintId::UnusedPub.id(), message, file.clone(), span.line).help(suggestion);
    // The specific blind spot beats the generic disclaimer: a shadowed
    // mention names the exact uncovered region (and the config entry that
    // would cover it); otherwise the standing caveat applies. TestOnly shares
    // the gate — an uncovered region could hold the production caller that
    // would retire the verdict. IntraCrate gets the narrowing flavor: the
    // mention may be a real use from OUTSIDE the crate (a bench, an
    // uncompiled target), so the `pub(crate)` tighten below is downgraded
    // too — auto-narrowing broke ripgrep's `cargo bench` in the 2026-07-10
    // validation (`is_match_candidate`, used in prod AND from a bench no
    // config compiled).
    let shadow = ctx.shadow_mention(&cand.id);
    let builder = match (verdict, shadow) {
        (PubVerdict::IntraCrate, Some(region)) => builder.note(shadow_narrow_note(region)),
        (_, Some(region)) => builder.note(shadow_report_note(region)),
        (_, None) => builder.note_once(
            "code compiled under configs outside `[engine] configs` and out-of-workspace consumers may cause false positives",
        ),
    };
    let (builder, removable) = apply_structural_fix(
        builder,
        cand,
        ctx.auto_delete,
        &file,
        span,
        verdict,
        shadow.is_some(),
    );
    (builder.build(), removable)
}

/// Structural fix policy — identical to the syn backend's:
///  - `IntraCrate` → `pub` → `pub(crate)`, `MachineApplicable` (the candidate
///    has an intra-crate referrer and has cleared every structural
///    must-stay-`pub` guard).
///  - `Unused` + `--fix-auto-delete` + git-tracked-clean → delete.
///  - `Unused` + `--fix-auto-delete` + dirty/untracked → deletion as
///    `MaybeIncorrect` plus an explanatory note.
///  - `Unused` without `--fix-auto-delete` → tighten as `MaybeIncorrect`: "unused"
///    still has residual blind spots (configs outside the matrix), so the
///    suggestion is shown but not auto-applied.
fn apply_structural_fix(
    builder: wl_diagnostic::builder::DiagnosticBuilder,
    cand: &PubCandidate,
    auto_delete: bool,
    file: &Path,
    span: &wl_ir::Span,
    verdict: PubVerdict,
    shadow_mentioned: bool,
) -> (wl_diagnostic::builder::DiagnosticBuilder, bool) {
    // Test-only reach gets NO structural suggestion on the plain path:
    // tightening trips `dead_code` on the plain build, and a bare deletion
    // would orphan the referencing tests (E0433/E0425 in `cargo test`) —
    // resolving it is a three-way human call the help text carries. In
    // auto-delete mode it falls through to the deletion surface below, which
    // ONLY the cascade acts on (gated behind the exclusive-test-scaffolding
    // proof; a blocked target's suggestion is downgraded with the blocker).
    if verdict == PubVerdict::TestOnly && !auto_delete {
        let builder = builder.note(
            "no fix is auto-applied: `pub(crate)` would trip `dead_code` on the non-test \
             build, and deleting the item would break the tests that reference it",
        );
        return (builder, false);
    }
    // The deletion surface is the WHOLE item (attrs/doc through body) — `span`
    // is rustc's `def_span`, only the signature, so deleting it would orphan a
    // function's body. `full_span` falls back to `span` only for the
    // no-editable-surface items that are never deletion candidates anyway.
    let delete_span = cand.full_span.as_ref().unwrap_or(span);
    if let Some((sugg, note)) = pick_deletion_fix(auto_delete, file, delete_span, verdict) {
        // Only a git-clean (recoverable) deletion is MachineApplicable — that
        // is the removal the cascade may build on.
        let removable = sugg.applicability == Applicability::MachineApplicable;
        let with_sugg = builder.suggestion(sugg);
        let builder = note.into_iter().fold(with_sugg, |b, reason| b.note(reason));
        return (builder, removable);
    }
    let builder = build_tighten_suggestion(cand, file, verdict, shadow_mentioned)
        .into_iter()
        .fold(builder, |b, s| b.suggestion(s));
    // `pub(crate)` compiles but trips `dead_code` (or a clippy lint the
    // exported status suppressed) on the plain build — say why `--fix`
    // won't apply it.
    let builder = if verdict == PubVerdict::IntraCrate && cand.intra_off_home {
        builder.note(
            "every use-site is gated behind a cfg the plain build never compiles \
             (`--target`-only or feature-gated code): `pub(crate)` would trip `dead_code` \
             on that build — narrow by hand if the gate doesn't apply to you",
        )
    } else if verdict == PubVerdict::IntraCrate && cand.dead_members {
        builder.note(
            "this trait declares members nothing calls: `pub(crate)` un-exempts it from \
             `dead_code`, which will flag them — remove the unused members first",
        )
    } else if verdict == PubVerdict::IntraCrate && cand.dead_fields {
        builder.note(
            "this type has a field nothing reads: `pub(crate)` un-exempts its fields from \
             `dead_code`, which will flag it — remove the write-only field first",
        )
    } else if verdict == PubVerdict::IntraCrate
        && let Some(unmask) = &cand.narrow_unmask
    {
        builder.note(format!(
            "`pub(crate)` would unmask clippy `{}` on `{}` (clippy exempts exported items \
             via `avoid-breaking-exported-api`) — resolve that first or narrow by hand",
            unmask.lint, unmask.member
        ))
    } else if verdict == PubVerdict::Unused && !auto_delete {
        // Without this the finding shows a naked `pub(crate)` diff that reads
        // as "will be auto-fixed" — say up front that `--fix` won't act on it.
        builder.note(
            "not auto-applied: deleting an unused item is `--fix-auto-delete` only — verify \
             it is truly unused, then delete it or narrow by hand",
        )
    } else {
        builder
    };
    (builder, false)
}

/// Build a suggestion that overwrites the item's visibility token with
/// `pub(crate)`. The byte range is the extractor's `vis_span` — rustc's own
/// token span mapped to on-disk offsets (see `wl_ir::Span`), verified
/// byte-exact against syn on every fixture the two share. `None` when there
/// is no editable token (macro-generated, trait-forced).
fn build_tighten_suggestion(
    cand: &PubCandidate,
    file: &Path,
    verdict: PubVerdict,
    shadow_mentioned: bool,
) -> Option<wl_diagnostic::Suggestion> {
    let span = cand.span.as_ref()?;
    let vis = cand.vis_span.as_ref()?;
    if vis.from_expansion {
        return None; // the token lives in a macro definition — not editable
    }
    let applicability = match verdict {
        // Reached only off the home config's cfg universe, a trait with
        // never-called members, or a narrow that would unmask a clippy lint
        // (`avoid-breaking-exported-api`): the narrow compiles but fails a
        // `-D warnings` gate — shown, never machine-applied (the data-common
        // `is_days`/`fraction`, ChronoExt, and `wrong_self_convention`
        // clusters from the 2026-07-05 LeaveDates validation).
        // A shadow mention joins the same class: the mention may be a real
        // use from outside the crate (bench source, uncovered cfg), so the
        // narrow could break code the engine never judged.
        PubVerdict::IntraCrate
            if cand.intra_off_home
                || cand.dead_members
                || cand.dead_fields
                || cand.narrow_unmask.is_some()
                || shadow_mentioned =>
        {
            Applicability::MaybeIncorrect
        }
        PubVerdict::IntraCrate => Applicability::MachineApplicable,
        PubVerdict::Unused => Applicability::MaybeIncorrect,
        // Never offered a tighten: `apply_structural_fix` returns before
        // building one (narrowing trips `dead_code` on the plain build).
        PubVerdict::TestOnly => return None,
    };
    // The existing visibility text for the rendered `-` diff line; falls back
    // to a placeholder if the file can't be read.
    let original = fs_err::read_to_string(file).ok().and_then(|src| {
        src.get(vis.lo as usize..vis.hi as usize)
            .map(str::to_string)
    });
    Some(wl_diagnostic::Suggestion {
        span: wl_diagnostic::Span {
            file: file.to_path_buf(),
            line_start: span.line,
            line_end: span.line,
            col_start: 1,
            col_end: 1,
            byte_start: vis.lo,
            byte_end: vis.hi,
        },
        message: "tighten to `pub(crate)`".into(),
        replacement: "pub(crate)".into(),
        applicability,
        original,
        // Filled in by `attach_pub_evidence` once the diagnostic is built.
    })
}

/// One crate-level hint suggesting `publish = true` for an internal crate
/// that produced `count` findings.
fn publish_hint(fast: &FastModel, krate: &CrateInfo, crate_code: &str, count: usize) -> Diagnostic {
    at_crate(
        LintId::UnusedPub.id(),
        format!("crate `{crate_code}` has {count} public items unused within the workspace"),
        // Workspace-relative like every other crate anchor — the metadata
        // manifest_dir is absolute and leaked the checkout prefix.
        fast.crate_relative_path(&krate.manifest_dir),
    )
    .help(format!(
        "if `{}` is published outside this workspace, set `publish = true` in its Cargo.toml \
         to treat its public API as external (these findings become exempt)",
        krate.name
    ))
    .note_once(
        "workspace-lint treats a crate as workspace-internal unless it declares `publish = true` \
         (or a registry); see the unused-pub docs",
    )
    .build()
}
