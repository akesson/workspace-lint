//! Visibility tightening — `pub` items only used inside their own crate
//! could be `pub(crate)` instead.
//!
//! Walks every workspace member's `pub` items and checks whether the item's
//! canonical path is referenced from any module in a *different* workspace
//! crate. Items with no cross-crate references are flagged.
//!
//! Known limitations (v1):
//!
//! - Items referenced via fully-qualified path (`my_crate::Foo::bar()`)
//!   instead of a `use` statement are not tracked.
//! - Items referenced only inside macro bodies are not tracked.
//! - Trait methods dispatched via `dyn Trait` are not tracked (would need
//!   type inference).

use std::collections::HashSet;

use syn_workspace::{Item, ItemKind, Module, ResolvedPath, Visibility as SynVisibility, Workspace};

use crate::diagnostic::Diagnostic;
use crate::diagnostic::Suggestion;
use crate::diagnostic::builder::at_line;
use crate::lints::{Lint, LintContext, LintId, Requirements};

pub(crate) struct Visibility;

impl Visibility {
    pub fn new() -> Self {
        Self
    }
}

impl Default for Visibility {
    fn default() -> Self {
        Self::new()
    }
}

impl Lint for Visibility {
    fn id(&self) -> LintId {
        LintId::Visibility
    }

    fn requirements(&self) -> Requirements {
        Requirements {
            needs_workspace: true,
        }
    }

    fn check(&self, cx: &LintContext<'_>) -> Vec<Diagnostic> {
        let workspace = cx
            .workspace
            .expect("visibility lint requires Workspace (Requirements::needs_workspace)");
        check(workspace)
    }
}

pub(crate) fn check(workspace: &Workspace) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    // Only inspect each member's primary unit — tests/benches/examples
    // legitimately use `pub` for cross-test plumbing.
    for (krate, target) in workspace.primary_units() {
        let code_name = krate.code_name();
        let macro_refs = workspace.macro_implicit_refs_for(krate);
        for (module, item) in target.root.walk_items() {
            if let Some(d) = check_item(workspace, module, item, &code_name, &macro_refs) {
                diagnostics.push(d);
            }
        }
    }
    diagnostics
}

fn check_item(
    workspace: &Workspace,
    module: &Module,
    item: &Item,
    crate_code_name: &str,
    macro_refs: &HashSet<ResolvedPath>,
) -> Option<Diagnostic> {
    if !item.kind.is_definition() || matches!(item.kind, ItemKind::Macro) {
        return None;
    }
    if item.visibility != SynVisibility::Public {
        return None;
    }
    if item.name == "main" && module.canonical.segments().len() == 1 {
        return None;
    }
    let cross_crate_used = workspace
        .referring_crates(&item.canonical)
        .is_some_and(|set| set.iter().any(|c| c != crate_code_name));
    if cross_crate_used {
        return None;
    }
    if macro_refs.contains(&item.canonical) {
        return None;
    }
    // Skip items reachable via a `pub use` chain in the workspace — those
    // are load-bearing for the re-export's compilation (E0364 / E0365) and
    // are part of the containing crate's public API surface even if no
    // in-workspace consumer references the re-exported name.
    if workspace.re_exports().is_target(&item.canonical) {
        return None;
    }
    // Skip items in a published library's public API surface — `pub fn`
    // in a `pub mod` in a `[lib]` crate is consumable by any downstream,
    // not just by in-workspace callers. The lint can't see external uses;
    // narrowing would silently break those consumers.
    if workspace.is_externally_reachable(&item.canonical) {
        return None;
    }

    let span = item.source.as_ref()?;
    let msg = format!(
        "pub `{}` in crate `{}` is not referenced from any other workspace crate",
        item.name, crate_code_name,
    );
    let mut builder = at_line(LintId::Visibility.id(), msg, span.file.clone(), span.line)
        .help("tighten to `pub(crate)` if this item is intentionally crate-internal")
        .note("references via fully-qualified path, trait dispatch, or proc-macro bodies are not tracked");
    if let Some(s) = build_tighten_suggestion(item) {
        builder = builder.suggestion(s);
    }
    Some(builder.build())
}

/// Build a `MachineApplicable` suggestion that overwrites the item's `pub`
/// keyword with `pub(crate)`. The byte range comes from
/// [`Item::vis_byte_range`], which is set by `syn-workspace` from the
/// `Visibility::Public` token's `proc-macro2` span — no source scanning,
/// no risk of matching a `pub` token inside doc comments or attribute
/// string literals. Returns `None` for items without a captured visibility
/// span (synthetic items, macros, or non-`pub` items that shouldn't be
/// reaching this code path anyway).
pub(crate) fn build_tighten_suggestion(item: &Item) -> Option<Suggestion> {
    let span = item.source.as_ref()?;
    let vis_range = item.vis_byte_range.clone()?;
    Some(Suggestion {
        span: crate::diagnostic::Span {
            file: span.file.clone(),
            line_start: span.line,
            line_end: span.line,
            col_start: 1,
            col_end: 1,
            byte_start: vis_range.start,
            byte_end: vis_range.end,
        },
        message: "tighten to `pub(crate)`".into(),
        replacement: "pub(crate)".into(),
        applicability: crate::diagnostic::Applicability::MachineApplicable,
    })
}
