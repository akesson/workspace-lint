//! Exclusive test scaffolding: the test items the unused-pub `--fix` cascade
//! may delete **alongside** a `TestOnly` target — and the blockers that veto
//! the target's deletion instead.
//!
//! A `TestOnly` item (reached only from test units) cannot be deleted alone:
//! the referencing tests would break (E0433/E0425 in `cargo test`). Deletion
//! is sound only when every referencing test item is *exclusive scaffolding*
//! — a test item whose every workspace edge lands in code that is also being
//! deleted — so target and scaffolding go together, or the target stays.
//!
//! The proof is an edge rule, not intent-guessing: a test fn that also
//! asserts on surviving code has an edge to it and is a blocker; a shared
//! fixture some surviving test still uses has a surviving inbound referrer
//! and is a blocker. Scaffolding may be mutually recursive (a test fn and the
//! private test helper only it calls), so membership is decided by a
//! **shrinking fixpoint** over an optimistic set: targets and their
//! referrer-closure start "in", and any member with an escape edge — or any
//! target with a demoted referrer — drops out until nothing changes. Every
//! rule fails toward keeping code.
//!
//! Unlike [`super::collateral`], there is **no causality gate**: a test item
//! is never "already dead" (the harness invokes it) — it is deleted only
//! because everything it exercises dies in this run, and that condition is
//! checked directly against this run's removal set.

use std::collections::{BTreeMap, BTreeSet};

use wl_ir::Span;

use super::assembly::{Assembly, Category};
use super::removal::RemovalSet;

/// The kinds a scaffold deletion may target — same list (and rationale) as
/// the private-collateral query: deleting an ADT would orphan its `impl`s,
/// and macro uses are only visible through expansions.
const DELETABLE_KINDS: &[&str] = &["fn", "const", "static", "type"];

/// One test item scheduled for deletion alongside the `TestOnly` target(s) it
/// exclusively scaffolds.
#[derive(Debug)]
pub struct TestScaffold {
    /// Cross-config identity (`crate::module::…::name`) — the removal-set key.
    pub id: String,
    /// Owning crate (code form). May be an integration-test target crate,
    /// which is *not* a workspace member name — resolve the owning member by
    /// span file when member config is needed.
    pub krate: String,
    /// The item's own name — the trailing `id` segment.
    pub name: String,
    /// rustc `DefKind` in the shared vocabulary.
    pub kind: String,
    /// Definition span — the diagnostic anchor.
    pub span: Option<Span>,
    /// Whole-item span — the delete surface.
    pub full_span: Option<Span>,
}

/// Why a `TestOnly` target must not be deleted: the test item that anchors it
/// to surviving code, and how.
#[derive(Debug)]
pub struct TestBlocker {
    /// The blocking test item's identity.
    pub test: String,
    /// Its definition span — the note's `file:line` anchor.
    pub span: Option<Span>,
    pub reason: BlockReason,
}

#[derive(Debug)]
pub enum BlockReason {
    /// The test item has a workspace edge to something that survives this fix
    /// — deleting it would drop real coverage (or a live helper call).
    ReachesSurviving { to: String },
    /// A surviving item still references the test item (a shared fixture /
    /// helper) — deleting it would break that survivor.
    KeptBySurvivor { from: String },
    /// The test item has no safe auto-delete surface (macro-generated span,
    /// ADT kind whose `impl`s would orphan, no whole-item span, …).
    NotDeletable,
}

/// The per-target partition the scaffolding query returns.
#[derive(Debug)]
pub enum ScaffoldVerdict {
    /// Every referencing test item is exclusive scaffolding: deleting the
    /// target requires deleting exactly these test items too.
    Cleared { scaffolding: Vec<TestScaffold> },
    /// A referencing test item escapes — the target must stay, and the
    /// blocker names why.
    Blocked(TestBlocker),
}

#[derive(Debug, Default)]
pub struct TestScaffolding {
    /// Target identity → this round's verdict. Every requested target gets an
    /// entry.
    pub per_target: BTreeMap<String, ScaffoldVerdict>,
}

/// A test item's display facts + its workspace edges, folded across configs.
#[derive(Default)]
struct TestItem<'a> {
    /// The best display def: prefers one with an editable surface (non-
    /// expansion span + `full_span`) so the macro-expanded `TestDescAndFn`
    /// const sharing a `#[test]` fn's path never shadows the fn itself.
    def: Option<&'a super::assembly::DefInfo>,
    /// Non-import, non-trait-scope outgoing edges to workspace identities.
    outgoing: BTreeSet<String>,
    /// Identities of items referencing this one (same edge classes), from any
    /// unit — a surviving referencer anywhere is an anchor.
    inbound: BTreeSet<String>,
}

pub(super) fn compute(
    configs: &[(String, Assembly)],
    removed: &RemovalSet,
    targets: &[String],
) -> TestScaffolding {
    let mut result = TestScaffolding::default();
    if targets.is_empty() {
        return result;
    }
    let target_set = RemovalSet::new(targets.iter());

    // Production identities: everything defined by a non-test unit anywhere
    // in the matrix. "Test item" = defined, but never by a production unit.
    let mut prod_ids: BTreeSet<String> = BTreeSet::new();
    for (_, asm) in configs {
        for frag in asm.archived_fragments() {
            if Assembly::is_test_unit(frag) {
                continue;
            }
            for it in frag.items.iter() {
                prod_ids.insert(wl_ir::join_paths(&it.path, "::"));
            }
        }
    }

    // Fold every test item's facts and edges across the matrix. Identities
    // are strings throughout — the same cross-config join the removal set
    // speaks — so foreign-generation targets (a `test = false` crate the
    // referring config never extracted) resolve exactly like local ones.
    let mut items: BTreeMap<String, TestItem<'_>> = BTreeMap::new();
    // Direct referrers of each target: the test items whose edges reach it.
    let mut referrers: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for (_, asm) in configs {
        for def in asm.defs.values() {
            if prod_ids.contains(&def.path) {
                continue;
            }
            let entry = items.entry(def.path.clone()).or_default();
            // Prefer the def with an editable delete surface (see TestItem).
            let editable = |d: &super::assembly::DefInfo| {
                d.full_span.is_some() && d.span.as_ref().is_some_and(|s| !s.from_expansion)
            };
            match entry.def {
                Some(prev) if editable(prev) || !editable(def) => {}
                _ => entry.def = Some(def),
            }
        }
        for frag in asm.archived_fragments() {
            let test_unit = Assembly::is_test_unit(frag);
            for e in frag.references.iter() {
                if e.import || e.trait_scope {
                    continue;
                }
                // Edges out of a compiler-synthesized def are harness
                // plumbing, not authored code: the `--test` harness's
                // generated `fn main` references every `#[test]` fn (via its
                // `TestDescAndFn` const) and would otherwise read as a
                // surviving referrer anchoring ALL scaffolding — worse, it
                // shares the bin's real `main` identity. Nothing synthetic
                // survives a deletion in any sense an author cares about.
                let from_def = asm.defs.get(e.from_key.as_str());
                if from_def.is_some_and(|d| d.synthetic) {
                    continue;
                }
                let Some(to_id) = asm.target_identity(e) else {
                    continue; // out-of-workspace (std, third-party): never binds
                };
                let from_id = from_def
                    .map(|d| d.path.clone())
                    .unwrap_or_else(|| wl_ir::join_paths(&e.from, "::"));
                let from_is_test = !prod_ids.contains(&from_id);
                if test_unit && from_is_test {
                    if target_set.covers(&segs(to_id)) {
                        for t in targets {
                            if covers_one(t, to_id) {
                                referrers.entry(t).or_default().insert(from_id.clone());
                            }
                        }
                    }
                    items
                        .entry(from_id.clone())
                        .or_default()
                        .outgoing
                        .insert(to_id.to_string());
                }
                if !prod_ids.contains(to_id) {
                    items
                        .entry(to_id.to_string())
                        .or_default()
                        .inbound
                        .insert(from_id);
                }
            }
        }
    }

    // The optimistic universe: the targets' direct referrers plus every
    // test-only identity they transitively reach (helpers at any depth).
    let mut universe: BTreeSet<String> = BTreeSet::new();
    let mut queue: Vec<String> = referrers.values().flatten().cloned().collect();
    while let Some(id) = queue.pop() {
        if !universe.insert(id.clone()) {
            continue;
        }
        if let Some(item) = items.get(&id) {
            for out in &item.outgoing {
                if items.contains_key(out)
                    && !universe.contains(out)
                    && !target_set.covers(&segs(out))
                    && !removed.covers(&segs(out))
                {
                    queue.push(out.clone());
                }
            }
        }
    }

    // The shrinking fixpoint. `in_s` holds targets ∪ scaffolding candidates;
    // a demotion shrinks it, which can strand a peer's edge (or a target's
    // referrer) and demote further — growth of `removed` between rounds only
    // ever *helps* exclusivity, so re-running per cascade round is monotone.
    let mut in_s: BTreeSet<String> = targets.iter().cloned().collect();
    in_s.extend(universe.iter().cloned());
    let mut demoted: BTreeMap<String, TestBlocker> = BTreeMap::new();
    let mut blocked: BTreeMap<String, String> = BTreeMap::new(); // target → blocking test id
    loop {
        let mut changed = false;
        let s_cover = RemovalSet::new(in_s.iter());
        let survives = |id: &str| s_cover.covers(&segs(id)) || removed.covers(&segs(id));
        for id in &universe {
            if !in_s.contains(id) {
                continue;
            }
            let item = &items[id];
            let reason = check_member(item, &survives);
            if let Some(reason) = reason {
                demoted.insert(
                    id.clone(),
                    TestBlocker {
                        test: id.clone(),
                        span: item.def.and_then(|d| d.span.clone()),
                        reason,
                    },
                );
                in_s.remove(id);
                changed = true;
            }
        }
        for t in targets {
            if !in_s.contains(t) {
                continue;
            }
            let anchor = referrers
                .get(t.as_str())
                .into_iter()
                .flatten()
                .find(|r| !in_s.contains(*r));
            if let Some(r) = anchor {
                blocked.insert(t.clone(), r.clone());
                in_s.remove(t);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // Report. A cleared target ships the surviving closure reachable from its
    // own referrers (per-target, so the caller can veto the target if any of
    // its scaffolds fails a lint-layer gate).
    for t in targets {
        if let Some(r) = blocked.get(t) {
            let b = demoted.remove(r).unwrap_or_else(|| TestBlocker {
                test: r.clone(),
                span: items
                    .get(r)
                    .and_then(|i| i.def)
                    .and_then(|d| d.span.clone()),
                reason: BlockReason::NotDeletable,
            });
            result
                .per_target
                .insert(t.clone(), ScaffoldVerdict::Blocked(b));
            continue;
        }
        let mut scaffolding_ids: BTreeSet<String> = BTreeSet::new();
        let mut queue: Vec<String> = referrers
            .get(t.as_str())
            .into_iter()
            .flatten()
            .filter(|r| in_s.contains(*r))
            .cloned()
            .collect();
        while let Some(id) = queue.pop() {
            if !scaffolding_ids.insert(id.clone()) {
                continue;
            }
            for out in &items[&id].outgoing {
                if universe.contains(out) && in_s.contains(out) && !scaffolding_ids.contains(out) {
                    queue.push(out.clone());
                }
            }
        }
        let scaffolding = scaffolding_ids
            .into_iter()
            .map(|id| {
                let def = items[&id]
                    .def
                    .expect("cleared scaffold passed the deletable check");
                TestScaffold {
                    krate: def.krate.clone(),
                    name: id.rsplit("::").next().unwrap_or(&id).to_string(),
                    kind: def.kind.clone(),
                    span: def.span.clone(),
                    full_span: def.full_span.clone(),
                    id,
                }
            })
            .collect();
        result
            .per_target
            .insert(t.clone(), ScaffoldVerdict::Cleared { scaffolding });
    }
    result
}

/// One member's demotion check: `None` = exclusive scaffolding (so far).
fn check_member(item: &TestItem<'_>, survives: &dyn Fn(&str) -> bool) -> Option<BlockReason> {
    let Some(def) = item.def else {
        return Some(BlockReason::NotDeletable);
    };
    if !DELETABLE_KINDS.contains(&def.kind.as_str())
        || def.synthetic
        || def.export_root
        || def.trait_item.is_some()
        || !matches!(def.category, Category::ModuleLevel | Category::InherentImpl)
        || def.full_span.is_none()
        || def.span.as_ref().is_none_or(|s| s.from_expansion)
    {
        return Some(BlockReason::NotDeletable);
    }
    if let Some(to) = item.outgoing.iter().find(|o| !survives(o)) {
        return Some(BlockReason::ReachesSurviving { to: to.clone() });
    }
    if let Some(from) = item.inbound.iter().find(|f| !survives(f)) {
        return Some(BlockReason::KeptBySurvivor { from: from.clone() });
    }
    None
}

/// Split an identity into the segment slice [`RemovalSet::covers`] takes.
fn segs(id: &str) -> Vec<&str> {
    id.split("::").collect()
}

/// Does identity `prefix` equal or segment-cover `id`?
fn covers_one(prefix: &str, id: &str) -> bool {
    let p = segs(prefix);
    let i = segs(id);
    i.len() >= p.len() && p.iter().zip(&i).all(|(a, b)| a == b)
}
