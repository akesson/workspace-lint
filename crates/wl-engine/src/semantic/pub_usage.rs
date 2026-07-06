//! Per-candidate unused-pub classification across the config matrix — the
//! query surface the ported `unused-pub` lint consumes.
//!
//! Where [`super::union::UnionVerdict`] reduces the matrix to lead/retired
//! *identities*, the lint needs the full per-candidate picture: the
//! cross-vs-intra usage split (drives the "tighten to `pub(crate)`" vs
//! "consider removing" message), the structural must-stay-`pub` guards
//! (re-export target, named in a public signature), the pub-module-hop
//! external reachability (the publish-gated exemption), and the spans the
//! diagnostic and its `--fix` suggestion anchor to. All of it is derived from
//! the fact union — no config knowledge lives here; kind/allowlist/path
//! filters and publish policy belong to the lint layer.

use std::collections::BTreeMap;

use wl_ir::Span;

use super::assembly::{Assembly, CANDIDATE_KINDS, Category, Reach};
use super::clippy_guard::NarrowUnmask;
use super::removal::{DegreeView, RemovalSet};

/// How a candidate is used, unioned across every extracted config. Mirrors the
/// syn lint's `Usage` vocabulary, plus the dispatch/export class the rustc
/// engine can see and syn couldn't.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PubUsage {
    /// A real (non-import) use-site in another crate under some config —
    /// integration tests, benches, and sibling bins compile as their own
    /// crates, so "keep it `pub`" cases land here exactly like the syn
    /// sibling-target rule. Leave alone.
    CrossCrate,
    /// Reached only through dispatch or linker export (external/internal trait
    /// dispatch, `#[no_mangle]`-style export roots). No direct edge to judge,
    /// but provably not dead — leave alone. (This class retires the
    /// `ffi_no_mangle_export` false positive syn couldn't avoid.)
    DispatchReached,
    /// Real use-sites exist but every one is in the owning crate — the
    /// "tighten to `pub(crate)`" advice is valid in every config that saw it.
    IntraCrate,
    /// No reaching edge under any config — the "appears unused" verdict.
    Unused,
}

/// One pub candidate with everything the lint needs to filter, classify, and
/// anchor a diagnostic. Spans come from the first config (matrix order) that
/// defines the item, so the primary config wins when it can see the def.
#[derive(Debug)]
pub struct PubCandidate {
    /// Cross-config identity: the definition path `crate::module::…::name`.
    pub id: String,
    /// Owning crate (code form) — the leading `id` segment.
    pub krate: String,
    /// The item's own name — the trailing `id` segment.
    pub name: String,
    /// rustc `DefKind` in the shared vocabulary (`fn`/`struct`/…).
    pub kind: String,
    pub category: Category,
    pub usage: PubUsage,
    /// Whole-definition span (on-disk byte offsets; see [`wl_ir::Span`]) — the
    /// diagnostic-anchor line (the signature).
    pub span: Option<Span>,
    /// Whole-**item** span (attrs/doc through body) — the `--fix` *delete*
    /// write surface (see [`wl_ir::ItemFact::full_span`]).
    pub full_span: Option<Span>,
    /// Visibility-token span — the `--fix` *tighten* write surface.
    pub vis_span: Option<Span>,
    /// Named in a `pub` item's signature under some config: tightening would
    /// not compile (E0446 / `private_interfaces`) — must stay `pub`.
    pub signature_exposed: bool,
    /// `pub`-module-hop reachable from outside the crate under some config —
    /// the substrate for the publish-gated external-API exemption.
    pub externally_reachable: bool,
    /// Target of a `use`/`pub use` under some config: tightening or deleting
    /// can break the re-export (E0364/E0365) — must stay `pub`.
    pub reexport_target: bool,
    /// `IntraCrate` reached ONLY outside the primary config (`--tests` /
    /// `--benches` cfg-gated code): `pub(crate)` still compiles, but the item
    /// is dead code on the plain build — `-D warnings` gates reject the
    /// narrowed tree, so the fix must not be machine-applied.
    pub test_only: bool,
    /// A trait with a member no primary-config edge reaches: narrowing
    /// un-exempts the trait from rustc `dead_code`, which then flags the
    /// never-called members — same `-D warnings` consequence, same gating.
    pub dead_members: bool,
    /// A type with a field no primary-config READ edge reaches: narrowing
    /// un-exempts its fields from rustc `dead_code`, which flags write-only
    /// fields — same `-D warnings` consequence, same gating.
    pub dead_fields: bool,
    /// Narrowing would activate a clippy lint the item's exported status
    /// currently suppresses (`avoid-breaking-exported-api`) — same
    /// `-D warnings` consequence, same gating. See [`NarrowUnmask`].
    pub narrow_unmask: Option<NarrowUnmask>,
}

/// The per-config fold of one identity: OR-accumulated over every
/// `DefPathHash` key sharing the definition path (a `--tests` compile emits
/// both the plain and `+test` cfg variants of a crate).
#[derive(Default)]
struct Fold {
    cross: bool,
    intra: bool,
    /// Intra reach seen in the PRIMARY config (matrix index 0) — its absence
    /// while `intra` holds means every use-site is test/bench cfg-gated.
    intra_primary: bool,
    dispatch: bool,
    signature_exposed: bool,
    externally_reachable: bool,
    reexport_target: bool,
    /// See [`PubCandidate::dead_members`] — primary-config judgement.
    dead_members: bool,
    /// See [`PubCandidate::dead_fields`] — primary-config judgement.
    dead_fields: bool,
}

pub(super) fn compute(configs: &[(String, Assembly)]) -> Vec<PubCandidate> {
    compute_impl(configs, None)
}

/// [`compute`] with a cascade removal set: each config's degrees are recomputed
/// as if `removed`'s defs had been deleted, so items a deleted item solely
/// reached surface as `Unused` in the *same* pass. Candidates whose own
/// identity is in `removed` are still returned (they read `Unused` once their
/// referrers vanish) — the caller dedups against what it has already scheduled.
pub(super) fn compute_excluding(
    configs: &[(String, Assembly)],
    removed: &RemovalSet,
) -> Vec<PubCandidate> {
    let overlays: Vec<_> = configs
        .iter()
        .map(|(_, a)| a.refold_excluding(removed))
        .collect();
    let views: Vec<DegreeView> = overlays.iter().map(|o| o.view()).collect();
    compute_impl(configs, Some(&views))
}

/// The shared fold. `overlay_views`, when present, supplies each config's
/// removal-recomputed degree source (index-aligned with `configs`); otherwise
/// the prebuilt indexes are used. Only the three removal-sensitive reads
/// (in/intra degree, signature exposure, and reach — which consults in-degree)
/// route through the view; `is_externally_reachable` / `is_import_target` are
/// invariant under item deletion and stay direct.
fn compute_impl(
    configs: &[(String, Assembly)],
    overlay_views: Option<&[DegreeView<'_>]>,
) -> Vec<PubCandidate> {
    let (_, primary) = &configs[0];
    let members = &primary.crates;
    let prebuilt: Vec<DegreeView>;
    let views: &[DegreeView] = match overlay_views {
        Some(vs) => vs,
        None => {
            prebuilt = configs.iter().map(|(_, a)| a.degree_view()).collect();
            &prebuilt
        }
    };

    // Union fold across configs, keyed by the cross-config identity. Def
    // display fields (spans, kind, category) come from the first config that
    // defines the identity — matrix order, primary first.
    let mut folds: BTreeMap<String, Fold> = BTreeMap::new();
    let mut display: BTreeMap<String, (&Assembly, String)> = BTreeMap::new();

    for (ci, (_, assembly)) in configs.iter().enumerate() {
        let view = &views[ci];
        for (key, def) in &assembly.defs {
            if !def.public
                || def.synthetic
                || !def.category.is_candidate()
                || !CANDIDATE_KINDS.contains(&def.kind.as_str())
                || !members.contains(&def.krate)
            {
                continue;
            }
            // A macro-generated def has no editable visibility token (its
            // span is projected to the invocation site, display-only), so it
            // is never an actionable unused-pub candidate. This also strips
            // the `--test` harness's public `TestDescAndFn` const generated
            // per `#[test]` fn, which shadows the (private) fn at the same
            // path and would otherwise flood the verdict.
            if def.span.as_ref().is_some_and(|s| s.from_expansion) {
                continue;
            }
            let fold = folds.entry(def.path.clone()).or_default();
            let workspace_wide = view.in_degree.get(key).copied().unwrap_or(0);
            let intra_only = view.intra_degree.get(key).copied().unwrap_or(0);
            fold.cross |= workspace_wide > intra_only;
            fold.intra |= intra_only > 0;
            if ci == 0 {
                fold.intra_primary |= intra_only > 0;
                if def.kind == "trait" {
                    fold.dead_members |= assembly.has_unreached_trait_member(key, view.in_degree);
                }
                if matches!(def.kind.as_str(), "struct" | "enum" | "union") {
                    fold.dead_fields |= assembly.has_unread_field(key, view.in_degree);
                }
            }
            fold.dispatch |= matches!(
                assembly.reach_with(key, def, view.in_degree),
                Reach::ExternalDispatch | Reach::InternalDispatch | Reach::ExportRoot
            );
            fold.signature_exposed |= view.signature_exposed.contains(key);
            fold.externally_reachable |= assembly.is_externally_reachable(key, def);
            fold.reexport_target |= assembly.is_import_target(key);
            display
                .entry(def.path.clone())
                .or_insert_with(|| (assembly, key.clone()));
        }
    }

    // Foreign reach: a config that never extracted the defining crate at all
    // (harness flag off) credits the identity directly — merged after the main
    // fold so it only ever ORs into identities some config actually defines
    // (a foreign-only identity has no display def and no candidate anyway).
    for view in views {
        for (id, fr) in view.foreign_reach {
            if let Some(fold) = folds.get_mut(id.as_str()) {
                fold.cross |= fr.cross;
                fold.intra |= fr.intra;
                fold.signature_exposed |= fr.signature_exposed;
            }
        }
    }

    folds
        .into_iter()
        .map(|(id, fold)| {
            let (assembly, key) = &display[&id];
            let def = &assembly.defs[key];
            let usage = if fold.cross {
                PubUsage::CrossCrate
            } else if fold.dispatch {
                PubUsage::DispatchReached
            } else if fold.intra {
                PubUsage::IntraCrate
            } else {
                PubUsage::Unused
            };
            PubCandidate {
                krate: def.krate.clone(),
                name: id.rsplit("::").next().unwrap_or(&id).to_string(),
                kind: def.kind.clone(),
                category: def.category,
                usage,
                narrow_unmask: assembly.narrow_unmask(key, def),
                span: def.span.clone(),
                full_span: def.full_span.clone(),
                vis_span: def.vis_span.clone(),
                signature_exposed: fold.signature_exposed,
                externally_reachable: fold.externally_reachable,
                reexport_target: fold.reexport_target,
                test_only: fold.intra && !fold.intra_primary && !fold.cross,
                dead_members: fold.dead_members,
                dead_fields: fold.dead_fields,
                id,
            }
        })
        .collect()
}
