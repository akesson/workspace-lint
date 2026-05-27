//! Visibility tightening — pub items that are only used inside their own
//! crate could be `pub(crate)` instead.
//!
//! Walks every workspace member's `pub` items and checks whether the item's
//! canonical path is referenced from any module in a *different* workspace
//! crate. Items with no cross-crate references are flagged as candidates
//! for `pub(crate)`. References are resolved through Tier 2.5's pub-use
//! chain index, so re-exported items still count as used when the
//! re-export itself is consumed cross-crate.
//!
//! Known limitations (v1) — documented in fixtures under
//! `known_false_positives/`:
//!
//! - Items referenced via fully-qualified path (`my_crate::Foo::bar()`)
//!   instead of a `use` statement are not tracked.
//! - Items referenced only inside macro bodies are not tracked.
//! - Trait methods dispatched via `dyn Trait` are not tracked (would need
//!   type inference).

use std::collections::HashSet;

use syn_workspace::{Item, ItemKind, Module, ResolvedPath, Visibility, Workspace};

use crate::diagnostic::Diagnostic;
use crate::diagnostic::builder::at_line;

pub const LINT: &str = "workspace-lint::visibility";

pub fn check(workspace: &Workspace) -> Vec<Diagnostic> {
    let cross_crate_refs = collect_cross_crate_refs(workspace);
    // Items reachable via macro-rules expansion should not be flagged even
    // if no `use` binding mentions them — Layer 1 autodetect collects
    // these workspace-wide.
    let macro_refs = workspace.macro_implicit_refs();
    let mut diagnostics = Vec::new();

    for krate in workspace.crates() {
        if !krate.is_workspace_member {
            continue;
        }
        let code_name = krate.name.replace('-', "_");
        collect_overpermissive(
            &krate.root,
            &code_name,
            &cross_crate_refs,
            macro_refs,
            &mut diagnostics,
        );
    }
    diagnostics
}

fn collect_overpermissive(
    module: &Module,
    crate_code_name: &str,
    cross_crate_refs: &HashSet<ResolvedPath>,
    macro_refs: &HashSet<ResolvedPath>,
    out: &mut Vec<Diagnostic>,
) {
    for item in &module.items {
        if !checkable(item) {
            continue;
        }
        if item.visibility != Visibility::Public {
            continue;
        }
        // The crate root's own `pub fn main()` is special — bins need pub
        // for cargo's entry-point resolution machinery.
        if item.name == "main" && module.canonical.segments().len() == 1 {
            continue;
        }
        if cross_crate_refs.contains(&item.canonical) {
            continue;
        }
        // Suppress if the item is reachable through any workspace
        // `macro_rules!` body — that's a real cross-crate use channel
        // even if no explicit `use` binding points at it.
        if macro_refs.contains(&item.canonical) {
            continue;
        }

        let Some(span) = &item.source else {
            continue;
        };
        let msg = format!(
            "pub `{}` in crate `{}` is not referenced from any other workspace crate",
            item.name, crate_code_name,
        );
        out.push(
            at_line(LINT, msg, span.file.clone(), span.line)
                .help("tighten to `pub(crate)` if this item is intentionally crate-internal")
                .note("references via fully-qualified path, trait dispatch, or proc-macro bodies are not tracked")
                .build(),
        );
    }
    for sub in &module.submodules {
        collect_overpermissive(sub, crate_code_name, cross_crate_refs, macro_refs, out);
    }
}

fn checkable(item: &Item) -> bool {
    matches!(
        item.kind,
        ItemKind::Fn
            | ItemKind::Struct
            | ItemKind::Enum
            | ItemKind::Union
            | ItemKind::Trait
            | ItemKind::TypeAlias
            | ItemKind::Const
            | ItemKind::Static
    )
}

fn collect_cross_crate_refs(workspace: &Workspace) -> HashSet<ResolvedPath> {
    let mut refs = HashSet::new();
    for krate in workspace.crates() {
        if !krate.is_workspace_member {
            continue;
        }
        let self_crate_code = krate.name.replace('-', "_");
        collect_refs_from_module(&krate.root, &self_crate_code, workspace, &mut refs);
    }
    refs
}

fn collect_refs_from_module(
    module: &Module,
    self_crate: &str,
    workspace: &Workspace,
    refs: &mut HashSet<ResolvedPath>,
) {
    for binding in &module.use_bindings {
        let canonical = workspace.resolve_canonical(&binding.canonical);
        if canonical.crate_name() != Some(self_crate) {
            refs.insert(canonical);
        }
    }
    for sub in &module.submodules {
        collect_refs_from_module(sub, self_crate, workspace, refs);
    }
}
