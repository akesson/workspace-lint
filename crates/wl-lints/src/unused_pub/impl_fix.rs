//! The cascade's handling of hand-written `impl` blocks (see the engine's
//! `impl_blocks` module for the rules): the veto when a dead type's impls
//! can't go with it, and the block deletions attached to the findings whose
//! removal empties or orphans a block.

use std::collections::HashMap;
use std::path::PathBuf;

use wl_diagnostic::Diagnostic;
use wl_engine::semantic::{DeletionUnmask, ImplBlockDeletion, ImplKeep};
use wl_lint_api::surgery::deletion::{DeleteOutcome, delete_suggestion};

/// Why the cascade keeps an otherwise-deletable item, decided on the trial
/// removal set of a round. Sticky once recorded (monotone, so the fixpoint
/// still terminates).
pub(super) enum Veto {
    /// Deleting it would unmask a `-D warnings` failure on a survivor.
    Unmask(DeletionUnmask),
    /// A dead type whose hand-written impls can't be deleted with it.
    Impl(ImplKeep),
}

pub(super) fn impl_keep_note(keep: &ImplKeep) -> String {
    match keep {
        ImplKeep::TraitImpl => "not auto-deleted: this type has a hand-written trait `impl` — \
             delete the type together with its impls by hand"
            .to_owned(),
        ImplKeep::NoEditableSurface => "not auto-deleted: this type has a macro-generated `impl` \
             block no edit can remove — delete the type and the macro input by hand"
            .to_owned(),
        ImplKeep::ItemReached { item } => format!(
            "not auto-deleted: `{item}`, in this type's `impl`, is still used by code that \
             survives this `--fix`"
        ),
    }
}

/// Attach each block deletion to the findings that cause it: the deleted
/// self type's, or — for a block its item deletions empty — every deleted
/// item's (overlapping pure deletions union at apply time, and an emptied
/// block goes only if all its items' deletions survive suppression).
pub(super) fn attach_block_deletions(
    findings: &mut [(Option<String>, Diagnostic)],
    blocks: &[ImplBlockDeletion],
) {
    let mut by_owner: HashMap<&str, Vec<&ImplBlockDeletion>> = HashMap::new();
    for b in blocks {
        if b.with_type {
            by_owner.entry(b.self_type.as_str()).or_default().push(b);
        } else {
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
