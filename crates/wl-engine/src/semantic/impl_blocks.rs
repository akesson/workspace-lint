//! Hand-written `impl` blocks as deletion units (schema 15).
//!
//! The `--fix-auto-delete` cascade deletes *items*; an impl block is not an
//! item, so before this module two shapes came out half done:
//!
//! - a dead type with a hand-written `impl T { … }` (even an EMPTY one) was
//!   never deleted — deleting the type alone would orphan the impl;
//! - an inherent impl whose every item the cascade deleted was left behind as
//!   an `impl T {}` husk.
//!
//! [`ImplFact::full_span`](wl_ir::ImplFact::full_span) and
//! [`ImplFact::items`](wl_ir::ImplFact::items) make the block itself a
//! deletion surface. The rules, failing toward keeping code:
//!
//! - a dead type may go **with** its written impls only if every one is an
//!   inherent impl (trait impls stay a veto — not judged here), every one has
//!   an editable span (not macro-generated), and no item inside one is
//!   reached from code that survives the deletion
//!   ([`SemanticModel::impls_keeping`]);
//! - a block is deleted when its self type is deleted, or when it is
//!   non-empty and every item in it is
//!   ([`SemanticModel::impl_block_deletions`]).
//!
//! A block appears once per cfg variant (plain and `+test` units both emit
//! it); variants are merged on the block's span, unioning their items — an
//! item only the `+test` variant compiles still has to go before the block
//! can.

use std::collections::{BTreeMap, BTreeSet};

use wl_ir::{ArchivedImplOrigin, Span};

use super::SemanticModel;
use super::removal::RemovalSet;

/// Why a dead type's hand-written impls keep it from being auto-deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImplKeep {
    /// An `impl Trait for T` — trait impls are not deleted by the cascade.
    TraitImpl,
    /// A macro-generated impl block: no editable span to delete.
    NoEditableSurface,
    /// An item inside an inherent impl is still used by code that survives.
    ItemReached { item: String },
}

/// One impl block the cascade deletes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImplBlockDeletion {
    /// The whole block — the delete surface.
    pub span: Span,
    /// The self type's identity.
    pub self_type: String,
    /// Deleted because the self type is; otherwise because every item in
    /// it is (the type survives).
    pub with_type: bool,
    /// Identities of the block's items (all of them deleted).
    pub items: Vec<String>,
}

/// One written impl block, merged across cfg variants.
struct Block {
    self_type: String,
    of_trait: bool,
    span: Option<Span>,
    items: Vec<String>,
}

impl SemanticModel {
    /// Every hand-written impl block of a workspace type, merged across cfg
    /// variants on the block span (a macro-generated block, span `None`, is
    /// kept separate per variant — it only ever vetoes).
    fn written_blocks(&self) -> Vec<Block> {
        let mut by_span: BTreeMap<(String, u32, u32), Block> = BTreeMap::new();
        let mut spanless: Vec<Block> = Vec::new();
        for (_, asm) in &self.configs {
            for frag in asm.archived_fragments() {
                for imp in frag.impls.iter() {
                    if !matches!(imp.origin, ArchivedImplOrigin::Written) {
                        continue;
                    }
                    let Some(self_def) = asm.defs.get(imp.self_key.as_str()) else {
                        continue;
                    };
                    let items: Vec<String> = imp
                        .items
                        .iter()
                        .map(|k| match asm.defs.get(k.as_str()) {
                            Some(d) => d.path.clone(),
                            // No local def (shouldn't happen for an own
                            // assoc item): an unresolvable item is never
                            // provably deleted, so it keeps the block.
                            None => format!("<unresolved {}>", k.as_str()),
                        })
                        .collect();
                    let span = imp
                        .full_span
                        .as_ref()
                        .map(Span::from)
                        .filter(|s| !s.from_expansion);
                    let block = Block {
                        self_type: self_def.path.clone(),
                        of_trait: imp.of_trait,
                        span: span.clone(),
                        items,
                    };
                    match span {
                        Some(s) => {
                            let key = (s.file.clone(), s.lo, s.hi);
                            match by_span.get_mut(&key) {
                                Some(slot) => {
                                    for it in block.items {
                                        if !slot.items.contains(&it) {
                                            slot.items.push(it);
                                        }
                                    }
                                }
                                None => {
                                    by_span.insert(key, block);
                                }
                            }
                        }
                        None => spanless.push(block),
                    }
                }
            }
        }
        by_span.into_values().chain(spanless).collect()
    }

    /// For each of `types` (dead types the cascade wants to delete, already
    /// in `trial`) that has hand-written impls, the reason those impls keep
    /// it — absent from the map when the type may go together with its
    /// impls. Reach is judged under `trial`'s overlay: an edge out of the
    /// type's own impls (or any other trial removal) no longer counts.
    pub fn impls_keeping(
        &self,
        trial: &RemovalSet,
        types: &[String],
    ) -> BTreeMap<String, ImplKeep> {
        let blocks = self.written_blocks();
        let mut out: BTreeMap<String, ImplKeep> = BTreeMap::new();
        let mut to_check: Vec<&Block> = Vec::new();
        for b in blocks.iter().filter(|b| types.contains(&b.self_type)) {
            if out.contains_key(&b.self_type) {
                continue;
            }
            if b.of_trait {
                out.insert(b.self_type.clone(), ImplKeep::TraitImpl);
            } else if b.span.is_none() {
                out.insert(b.self_type.clone(), ImplKeep::NoEditableSurface);
            } else {
                to_check.push(b);
            }
        }
        to_check.retain(|b| !out.contains_key(&b.self_type));
        if to_check.is_empty() {
            return out;
        }
        let wanted: BTreeSet<&str> = to_check
            .iter()
            .flat_map(|b| b.items.iter().map(String::as_str))
            .collect();
        for (_, asm) in &self.configs {
            let overlay = asm.refold_excluding(trial);
            let view = overlay.view();
            // This config's keys for the items in question (several cfg
            // variants may each define one).
            let mut keys: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
            for (k, d) in asm.defs.iter() {
                if let Some(&id) = wanted.get(d.path.as_str()) {
                    keys.entry(id).or_default().push(k.as_str());
                }
            }
            let reached = |id: &str| {
                keys.get(id).is_some_and(|ks| {
                    ks.iter()
                        .any(|k| view.in_degree.get(*k).copied().unwrap_or(0) > 0)
                }) || view
                    .foreign_reach
                    .get(id)
                    .is_some_and(|f| f.cross || f.intra)
            };
            for b in &to_check {
                if out.contains_key(&b.self_type) {
                    continue;
                }
                if let Some(item) = b.items.iter().find(|it| reached(it)) {
                    out.insert(
                        b.self_type.clone(),
                        ImplKeep::ItemReached { item: item.clone() },
                    );
                }
            }
        }
        out
    }

    /// The hand-written inherent impl blocks deleting `removed` takes with
    /// it: those of a removed type, and every non-empty block whose items
    /// are all removed (the `impl T {}` husk otherwise left behind).
    pub fn impl_block_deletions(&self, removed: &RemovalSet) -> Vec<ImplBlockDeletion> {
        let gone = |id: &str| {
            let segs: Vec<&str> = id.split("::").collect();
            removed.covers(&segs)
        };
        self.written_blocks()
            .into_iter()
            .filter(|b| !b.of_trait)
            .filter_map(|b| {
                let span = b.span?;
                let with_type = removed.contains_id(&b.self_type);
                let emptied = !b.items.is_empty() && b.items.iter().all(|it| gone(it));
                (with_type || emptied).then_some(ImplBlockDeletion {
                    span,
                    self_type: b.self_type,
                    with_type,
                    items: b.items,
                })
            })
            .collect()
    }
}
