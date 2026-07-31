//! Second-order **private** dead code: the items rustc's `dead_code` would
//! start flagging once an unused-pub `--fix` cascade deletes their last user.
//!
//! The cascade itself only removes `pub` candidates — but a deleted pub item's
//! body was often the sole caller of some private helper, and the fixed tree
//! then fails a `-D warnings` gate on `dead_code` (the 2026-07-05 LeaveDates
//! validation's "now-dead private helpers" class). This query surfaces those
//! helpers so the cascade can delete them in the same pass.
//!
//! Scope is deliberately narrow, erring toward *leaving* code:
//!
//! - **Causality-gated**: only defs that HAD a reaching edge before the
//!   removal and have none after it, in every extracted config. A private def
//!   with no edges at all was dead before this fix — on a warning-clean
//!   baseline that means its users live in cfg universes the engine never
//!   extracts (or it carries `#[allow(dead_code)]`), and it is the author's,
//!   not ours.
//! - **Kind-limited** to `fn`/`const`/`static`/`type`: deleting a private
//!   struct or enum would orphan its `impl` blocks (which are not tree items
//!   the IR models as deletable), and macro uses are only visible through
//!   their expansions — both stay.
//! - The usual editability guards: not synthetic, not macro-generated, a
//!   `full_span` to delete, no export-shaped attrs (a `#[no_mangle]` item has
//!   no Rust referrer by design), not a trait member (visibility trait-forced).
//!
//! Struct *fields* stay out entirely: the IR has no field defs, and deleting
//! one would require editing every construction site — a now-dead private
//! field remains an author decision (documented in the README's `--fix`
//! caveats).

use std::collections::BTreeMap;

use wl_ir::Span;

use super::assembly::Assembly;
use super::removal::RemovalSet;

/// The kinds a collateral deletion may target — see the module docs for why
/// ADTs and macros are excluded.
const DELETABLE_KINDS: &[&str] = &["fn", "const", "static", "type"];

/// One private def whose last reaching edge originates in a removed def — a
/// deletion the cascade should carry out alongside the removal that caused it.
#[derive(Debug)]
pub struct PrivateOrphan {
    /// Cross-config identity (`crate::module::…::name`) — the removal-set key.
    pub id: String,
    /// Owning crate (code form).
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

/// Per-identity fold across the config matrix: reached before the removal
/// (somewhere), reached after it (anywhere).
#[derive(Default)]
struct Fold {
    used_pre: bool,
    alive_post: bool,
}

pub(super) fn compute(configs: &[(String, Assembly)], removed: &RemovalSet) -> Vec<PrivateOrphan> {
    let members = &configs[0].1.crates;
    let overlays: Vec<_> = configs
        .iter()
        .map(|(_, a)| a.refold_excluding(removed))
        .collect();

    let mut folds: BTreeMap<String, Fold> = BTreeMap::new();
    let mut display: BTreeMap<String, (&Assembly, String)> = BTreeMap::new();
    for (ci, (_, assembly)) in configs.iter().enumerate() {
        let pre = assembly.degree_view();
        let post = overlays[ci].view();
        for (key, def) in &assembly.defs {
            if def.public
                || def.synthetic
                || def.export_root
                || !def.category.supports_deletion()
                || !DELETABLE_KINDS.contains(&def.kind.as_str())
                || !members.contains(&def.krate)
                || def.span.as_ref().is_none_or(|s| s.from_expansion)
                || def.full_span.is_none()
            {
                continue;
            }
            let fold = folds.entry(def.path.clone()).or_default();
            fold.used_pre |= pre.in_degree.get(key).copied().unwrap_or(0) > 0;
            fold.alive_post |= post.in_degree.get(key).copied().unwrap_or(0) > 0;
            display
                .entry(def.path.clone())
                .or_insert_with(|| (assembly, key.clone()));
        }
    }

    // Identity-level reach from configs that never extracted the defining
    // crate keeps the def alive / marks it pre-used, exactly as the pub
    // verdict treats foreign reach. Merged after the main fold so it only
    // ever ORs into identities some config actually defines.
    for (ci, (_, assembly)) in configs.iter().enumerate() {
        for (id, fr) in &assembly.degrees.foreign_reach {
            if (fr.cross || fr.intra)
                && let Some(fold) = folds.get_mut(id.as_str())
            {
                fold.used_pre = true;
            }
        }
        for (id, fr) in &overlays[ci].foreign_reach {
            if (fr.cross || fr.intra)
                && let Some(fold) = folds.get_mut(id.as_str())
            {
                fold.alive_post = true;
            }
        }
    }

    folds
        .into_iter()
        .filter(|(_, fold)| fold.used_pre && !fold.alive_post)
        .map(|(id, _)| {
            let (assembly, key) = &display[&id];
            let def = &assembly.defs[key];
            PrivateOrphan {
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
