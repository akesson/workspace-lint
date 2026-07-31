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

use super::assembly::Assembly;
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
    let graph = Graph::fold(configs, targets);
    let universe = graph.referrer_closure(removed);
    let (in_s, mut demoted, blocked) = graph.shrink(&universe, targets, removed);
    for t in targets {
        let verdict = match blocked.get(t) {
            Some(r) => ScaffoldVerdict::Blocked(graph.take_blocker(&mut demoted, r)),
            None => ScaffoldVerdict::Cleared {
                scaffolding: graph.cleared_closure(t, &universe, &in_s),
            },
        };
        result.per_target.insert(t.clone(), verdict);
    }
    result
}

/// The test-item graph one scaffolding query works over: every test-only
/// identity's display def and workspace edges, folded across the config
/// matrix, plus each target's direct referrers. Identities are strings
/// throughout — the same cross-config join the removal set speaks — so
/// foreign-generation targets (a `test = false` crate the referring config
/// never extracted) resolve exactly like local ones.
struct Graph<'a> {
    target_set: RemovalSet,
    items: BTreeMap<String, TestItem<'a>>,
    /// Target identity → the test items whose edges reach it.
    referrers: BTreeMap<String, BTreeSet<String>>,
}

impl<'a> Graph<'a> {
    fn fold(configs: &'a [(String, Assembly)], targets: &[String]) -> Self {
        let prod_ids = prod_identities(configs);
        let mut g = Graph {
            target_set: RemovalSet::new(targets.iter()),
            items: BTreeMap::new(),
            referrers: BTreeMap::new(),
        };
        for (_, asm) in configs {
            for def in asm.defs.values() {
                if !prod_ids.contains(&def.path) {
                    g.record_def(def);
                }
            }
            for frag in asm.archived_fragments() {
                let test_unit = Assembly::is_test_unit(frag);
                for e in frag.references.iter() {
                    g.record_edge(asm, e, test_unit, &prod_ids, targets);
                }
            }
        }
        g
    }

    /// Fold one test-item def, preferring the one with an editable delete
    /// surface (see [`TestItem::def`]).
    fn record_def(&mut self, def: &'a super::assembly::DefInfo) {
        let editable = |d: &super::assembly::DefInfo| {
            d.full_span.is_some() && d.span.as_ref().is_some_and(|s| !s.from_expansion)
        };
        let entry = self.items.entry(def.path.clone()).or_default();
        match entry.def {
            Some(prev) if editable(prev) || !editable(def) => {}
            _ => entry.def = Some(def),
        }
    }

    /// Fold one reference edge into the outgoing/inbound/referrer tables.
    fn record_edge(
        &mut self,
        asm: &Assembly,
        e: &wl_ir::ArchivedRefEdge,
        test_unit: bool,
        prod_ids: &BTreeSet<String>,
        targets: &[String],
    ) {
        if e.import || e.trait_scope {
            return;
        }
        // Edges out of a compiler-synthesized def are harness plumbing, not
        // authored code: the `--test` harness's generated `fn main`
        // references every `#[test]` fn (via its `TestDescAndFn` const) and
        // would otherwise read as a surviving referrer anchoring ALL
        // scaffolding — worse, it shares the bin's real `main` identity.
        // Nothing synthetic survives a deletion in any sense an author cares
        // about.
        let from_def = asm.defs.get(e.from_key.as_str());
        if from_def.is_some_and(|d| d.synthetic) {
            return;
        }
        let Some(to_id) = asm.target_identity(e) else {
            return; // out-of-workspace (std, third-party): never binds
        };
        let from_id = from_def
            .map(|d| d.path.clone())
            .unwrap_or_else(|| wl_ir::join_paths(&e.from, "::"));
        if test_unit && !prod_ids.contains(&from_id) {
            if self.target_set.covers(&segs(to_id)) {
                for t in targets.iter().filter(|t| covers_one(t, to_id)) {
                    self.referrers
                        .entry(t.clone())
                        .or_default()
                        .insert(from_id.clone());
                }
            }
            self.items
                .entry(from_id.clone())
                .or_default()
                .outgoing
                .insert(to_id.to_string());
        }
        if !prod_ids.contains(to_id) {
            self.items
                .entry(to_id.to_string())
                .or_default()
                .inbound
                .insert(from_id);
        }
    }

    /// The optimistic universe: the targets' direct referrers plus every
    /// test-only identity they transitively reach (helpers at any depth).
    fn referrer_closure(&self, removed: &RemovalSet) -> BTreeSet<String> {
        let mut universe: BTreeSet<String> = BTreeSet::new();
        let mut queue: Vec<String> = self.referrers.values().flatten().cloned().collect();
        while let Some(id) = queue.pop() {
            if !universe.insert(id.clone()) {
                continue;
            }
            let Some(item) = self.items.get(&id) else {
                continue;
            };
            for out in &item.outgoing {
                if self.items.contains_key(out)
                    && !universe.contains(out)
                    && !self.target_set.covers(&segs(out))
                    && !removed.covers(&segs(out))
                {
                    queue.push(out.clone());
                }
            }
        }
        universe
    }

    /// The shrinking fixpoint. `in_s` holds targets ∪ scaffolding candidates;
    /// a demotion shrinks it, which can strand a peer's edge (or a target's
    /// referrer) and demote further — growth of `removed` between rounds only
    /// ever *helps* exclusivity, so re-running per cascade round is monotone.
    /// Returns the surviving set, the demoted members (with their reasons),
    /// and the blocked targets (→ blocking test id).
    fn shrink(
        &self,
        universe: &BTreeSet<String>,
        targets: &[String],
        removed: &RemovalSet,
    ) -> (
        BTreeSet<String>,
        BTreeMap<String, TestBlocker>,
        BTreeMap<String, String>,
    ) {
        let mut in_s: BTreeSet<String> = targets.iter().cloned().collect();
        in_s.extend(universe.iter().cloned());
        let mut demoted: BTreeMap<String, TestBlocker> = BTreeMap::new();
        let mut blocked: BTreeMap<String, String> = BTreeMap::new();
        loop {
            let mut changed = self.demote_members(universe, removed, &mut in_s, &mut demoted);
            changed |= self.demote_targets(targets, &mut in_s, &mut blocked);
            if !changed {
                break;
            }
        }
        (in_s, demoted, blocked)
    }

    /// One demotion pass over the scaffolding candidates still in `in_s`.
    fn demote_members(
        &self,
        universe: &BTreeSet<String>,
        removed: &RemovalSet,
        in_s: &mut BTreeSet<String>,
        demoted: &mut BTreeMap<String, TestBlocker>,
    ) -> bool {
        let s_cover = RemovalSet::new(in_s.iter());
        let survives = |id: &str| s_cover.covers(&segs(id)) || removed.covers(&segs(id));
        let mut changed = false;
        for id in universe {
            if !in_s.contains(id) {
                continue;
            }
            let item = &self.items[id];
            if let Some(reason) = check_member(item, &survives) {
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
        changed
    }

    /// One demotion pass over the targets: a target with a demoted direct
    /// referrer is blocked (the referrer stays alive, so the target must).
    fn demote_targets(
        &self,
        targets: &[String],
        in_s: &mut BTreeSet<String>,
        blocked: &mut BTreeMap<String, String>,
    ) -> bool {
        let mut changed = false;
        for t in targets {
            if !in_s.contains(t) {
                continue;
            }
            let anchor = self
                .referrers
                .get(t)
                .into_iter()
                .flatten()
                .find(|r| !in_s.contains(*r));
            if let Some(r) = anchor {
                blocked.insert(t.clone(), r.clone());
                in_s.remove(t);
                changed = true;
            }
        }
        changed
    }

    /// The blocker report for a blocked target: the demoted referrer's own
    /// record, or a synthesized one when the referrer was never admitted.
    fn take_blocker(&self, demoted: &mut BTreeMap<String, TestBlocker>, r: &str) -> TestBlocker {
        demoted.remove(r).unwrap_or_else(|| TestBlocker {
            test: r.to_string(),
            span: self
                .items
                .get(r)
                .and_then(|i| i.def)
                .and_then(|d| d.span.clone()),
            reason: BlockReason::NotDeletable,
        })
    }

    /// A cleared target's scaffolding: the surviving closure reachable from
    /// its own referrers (per-target, so the caller can veto the target if
    /// any of its scaffolds fails a lint-layer gate).
    fn cleared_closure(
        &self,
        target: &str,
        universe: &BTreeSet<String>,
        in_s: &BTreeSet<String>,
    ) -> Vec<TestScaffold> {
        let mut ids: BTreeSet<String> = BTreeSet::new();
        let mut queue: Vec<String> = self
            .referrers
            .get(target)
            .into_iter()
            .flatten()
            .filter(|r| in_s.contains(*r))
            .cloned()
            .collect();
        while let Some(id) = queue.pop() {
            if !ids.insert(id.clone()) {
                continue;
            }
            for out in &self.items[&id].outgoing {
                if universe.contains(out) && in_s.contains(out) && !ids.contains(out) {
                    queue.push(out.clone());
                }
            }
        }
        ids.into_iter()
            .map(|id| {
                let def = self.items[&id]
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
            .collect()
    }
}

/// Every identity defined by a non-test unit anywhere in the matrix. "Test
/// item" = defined, but never by a production unit.
fn prod_identities(configs: &[(String, Assembly)]) -> BTreeSet<String> {
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
    prod_ids
}

/// One member's demotion check: `None` = exclusive scaffolding (so far).
fn check_member(item: &TestItem<'_>, survives: &dyn Fn(&str) -> bool) -> Option<BlockReason> {
    let Some(def) = item.def else {
        return Some(BlockReason::NotDeletable);
    };
    if !DELETABLE_KINDS.contains(&def.kind.as_str())
        || def.synthetic
        || def.export_root
        || !def.category.supports_deletion()
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
