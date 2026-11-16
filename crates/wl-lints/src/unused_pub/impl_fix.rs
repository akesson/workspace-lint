//! The cascade's handling of hand-written `impl` blocks (see the engine's
//! `impl_blocks` module for the rules): the vetoes when a dead type's block
//! can't be deleted with it, and the block deletions attached to the
//! findings whose removal empties or orphans a block.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use wl_diagnostic::Diagnostic;
use wl_engine::semantic::{ImplBlockDeletion, RemovalSet};
use wl_lint_api::surgery::deletion::{DeleteOutcome, delete_suggestion};

use super::scope::{FindingScope, shadow_block_note};
use super::veto::{Veto, VetoCtx};

impl VetoCtx<'_> {
    /// For each of `types` (dead types in `trial`), a veto when one of the
    /// blocks that would go with it can't be deleted safely — deleting the
    /// type while its block stays would orphan the block (E0412):
    /// - code inside it no declared config compiles (unseen by the IR, so
    ///   deleting the block would delete it unjudged);
    /// - its file is out of scope (generated, `exclude-paths`) or has no git
    ///   backup (dirty, untracked).
    pub(super) fn block_vetoes(&self, trial: &RemovalSet, types: &[String]) -> Vec<(String, Veto)> {
        let types: HashSet<&str> = types.iter().map(String::as_str).collect();
        let mut out: Vec<(String, Veto)> = Vec::new();
        for b in self.model.impl_block_deletions(trial) {
            if !b.with_type
                || !types.contains(b.self_type.as_str())
                || out.iter().any(|(id, _)| *id == b.self_type)
            {
                continue;
            }
            if let Some(note) = self.uneditable(&b) {
                out.push((b.self_type, Veto::Block(note)));
            }
        }
        out
    }

    /// Why `block` can't be deleted, if it can't.
    fn uneditable(&self, block: &ImplBlockDeletion) -> Option<String> {
        if let Some(region) = self.shadowed(block) {
            return Some(shadow_block_note(region));
        }
        const UNEDITABLE: &str = "not auto-deleted: this type's `impl` block can't be edited \
             — delete the type and its impl by hand";
        let krate_code = block.self_type.split("::").next().unwrap_or_default();
        let Some(krate) = self
            .fast
            .members()
            .iter()
            .find(|k| k.code_name() == krate_code)
        else {
            return Some(UNEDITABLE.to_owned()); // fail toward keeping
        };
        let config = self.config.for_crate(&krate.name);
        let scope = FindingScope::new(config, self.fast, krate, self.generated, None);
        if scope.skips(&block.self_type, "impl", Some(&block.span)) {
            return Some(
                "not auto-deleted: this type's `impl` block is in a generated or excluded \
                 file the fix must not edit — delete the type and its impl by hand"
                    .to_owned(),
            );
        }
        match delete_suggestion(&PathBuf::from(&block.span.file), &block.span) {
            DeleteOutcome::Apply(_) => None,
            DeleteOutcome::Skip(_, reason) => Some(format!(
                "not auto-deleted: this type's `impl` block can't go with it — {reason}"
            )),
            DeleteOutcome::Unavailable => Some(UNEDITABLE.to_owned()),
        }
    }

    fn shadowed(&self, block: &ImplBlockDeletion) -> Option<&wl_engine::coverage::ShadowRegion> {
        self.shadow.region_within(
            &block.span.file,
            block.span.lo as usize,
            block.span.hi as usize,
        )
    }

    /// Attach each block deletion to the findings that cause it: the deleted
    /// self type's, or — for a block its item deletions empty — every deleted
    /// item's (overlapping pure deletions union at apply time). Every item of
    /// an emptied block was seeded by the cascade, which seeds only findings
    /// no directive silences, so the findings carrying the block survive
    /// suppression together. An emptied block holding code no config
    /// compiles is left in place (its unseen items keep it non-empty); a
    /// with-type block was cleared by [`Self::block_vetoes`] already.
    pub(super) fn attach_block_deletions(
        &self,
        findings: &mut [(Option<String>, Diagnostic)],
        removed: &RemovalSet,
    ) {
        let blocks = self.model.impl_block_deletions(removed);
        let mut by_owner: HashMap<&str, Vec<&ImplBlockDeletion>> = HashMap::new();
        for b in &blocks {
            if b.with_type {
                by_owner.entry(b.self_type.as_str()).or_default().push(b);
            } else if self.shadowed(b).is_none() {
                for it in &b.items {
                    by_owner.entry(it.as_str()).or_default().push(b);
                }
            }
        }
        for (id, d) in findings.iter_mut() {
            let Some(blocks) = id.as_deref().and_then(|id| by_owner.get(id)) else {
                continue;
            };
            for b in blocks {
                let file = PathBuf::from(&b.span.file);
                if let DeleteOutcome::Apply(s) = delete_suggestion(&file, &b.span) {
                    d.suggestions.push(s);
                }
            }
        }
    }
}
