//! Why the `--fix-auto-delete` cascade keeps an otherwise-deletable item.

use std::collections::HashSet;
use std::path::PathBuf;

use wl_engine::coverage::CfgShadow;
use wl_engine::fast::FastModel;
use wl_engine::semantic::{DeletionUnmask, ImplKeep, RemovalSet, SemanticModel};
use wl_lint_api::config::PerCrate;

use super::config::UnusedPubConfig;

/// A deletion veto, judged per cascade round on that round's trial removal
/// set (reach can vanish when a later round deletes the reaching code, so a
/// veto is never carried over — the loop still terminates: every round that
/// doesn't converge grows the removal set).
pub(super) enum Veto {
    /// Deleting it would unmask a `-D warnings` failure on a survivor.
    Unmask(DeletionUnmask),
    /// A dead type whose hand-written impls can't be deleted with it.
    Impl(ImplKeep),
    /// A dead type whose (deletable) impl block can't be edited safely: its
    /// file is dirty/untracked or generated/excluded, or it holds code no
    /// config compiles. The note is the reason.
    Block(String),
    /// Deleting it together with the rest of the trial would leave the
    /// `pub use …::*` glob re-exporting this module re-exporting nothing.
    Glob { module: String },
}

/// What a cascade round's veto stage asks: which of `trial_new` — deleted
/// on top of `removed` — must stay, and why. A seam so the round's fixpoint
/// can be tested against vetoes that depend on the trial's composition.
pub(super) trait VetoOracle {
    fn vetoes(&self, removed: &HashSet<String>, trial_new: &[String]) -> Vec<(String, Veto)>;
}

/// Everything judging a deletion veto consults: the engine's trial queries,
/// the cfg-shadow index, and the per-crate scope/git gates (the impl-block
/// checks live in `impl_fix`).
pub(super) struct VetoCtx<'a> {
    pub(super) model: &'a SemanticModel,
    pub(super) shadow: &'a CfgShadow,
    pub(super) config: &'a PerCrate<UnusedPubConfig>,
    pub(super) fast: &'a FastModel,
    pub(super) generated: &'a HashSet<PathBuf>,
}

impl VetoOracle for VetoCtx<'_> {
    fn vetoes(&self, removed: &HashSet<String>, trial_new: &[String]) -> Vec<(String, Veto)> {
        let trial = RemovalSet::new(removed.iter().chain(trial_new.iter()));
        // One overlay shared by both engine queries (a refold per config is
        // the expensive step).
        let overlay = self.model.trial_overlay(&trial);
        let mut vetoes: Vec<(String, Veto)> = self
            .model
            .deletion_unmasks(&overlay, trial_new)
            .into_iter()
            .map(|(id, u)| (id, Veto::Unmask(u)))
            .chain(
                self.model
                    .impls_keeping(&overlay, trial_new)
                    .into_iter()
                    .map(|(id, k)| (id, Veto::Impl(k))),
            )
            .chain(self.block_vetoes(&trial, trial_new))
            .collect();
        // The glob check last, on the trial WITHOUT the items just kept: a
        // kept sibling keeps the glob non-empty. (The fixpoint never takes a
        // veto back, so judging it on the full trial would keep, with a
        // false note, a sibling that may go.)
        let kept: HashSet<&str> = vetoes.iter().map(|(id, _)| id.as_str()).collect();
        let glob_new: Vec<String> = trial_new
            .iter()
            .filter(|id| !kept.contains(id.as_str()))
            .cloned()
            .collect();
        let glob_trial = RemovalSet::new(removed.iter().chain(glob_new.iter()));
        let globs = self.model.glob_emptied(&glob_trial, &glob_new);
        vetoes.extend(
            globs
                .into_iter()
                .map(|(id, module)| (id, Veto::Glob { module })),
        );
        vetoes
    }
}

impl Veto {
    /// The note the kept finding carries: what would break, and what the
    /// human can do instead.
    pub(super) fn note(&self) -> String {
        match self {
            Self::Unmask(u) => unmask_note(u),
            Self::Impl(k) => impl_keep_note(k),
            Self::Block(note) => note.clone(),
            Self::Glob { module } => format!(
                "not auto-deleted: `{module}` is re-exported through a `pub use …::*` glob, \
                 and this fix would delete every public item in it — the glob would \
                 re-export nothing (`unused_imports`); delete them by hand together with \
                 the glob"
            ),
        }
    }
}

/// Names the exact warning the deletion would activate on a survivor, in the
/// style of the narrow-guard notes (`ir.rs`).
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
        DeletionUnmask::UnconstructedVariant { owner, variant } => format!(
            "deleting this would leave variant `{variant}` of surviving `{owner}` never \
             constructed, tripping `dead_code` on the fixed tree — remove the variant first \
             or delete by hand"
        ),
    }
}

fn impl_keep_note(keep: &ImplKeep) -> String {
    match keep {
        ImplKeep::TraitImpl => "not auto-deleted: this type has a hand-written trait `impl` — \
             delete the type together with its impls by hand"
            .to_owned(),
        ImplKeep::NoEditableSurface => "not auto-deleted: this type has a macro-generated `impl` \
             that deleting the type's own source item wouldn't remove — delete the type \
             and the macro input by hand"
            .to_owned(),
        ImplKeep::ItemReached { item } => format!(
            "not auto-deleted: `{item}`, in this type's `impl`, is still used by code that \
             survives this `--fix`"
        ),
    }
}
