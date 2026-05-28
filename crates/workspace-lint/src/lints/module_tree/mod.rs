//! Module-tree integrity — orphan files and dangling `mod` declarations.
//!
//! Two structural failure modes:
//!
//! - **mod_decl_missing_target**: `mod foo;` appears in source but neither
//!   `foo.rs` nor `foo/mod.rs` exists adjacent to the parent file, and no
//!   `#[path = "..."]` override resolves either. Recorded by syn-workspace's
//!   `BrokenModDecl` during the Tier 2 walk.
//! - **orphan_rs_file**: a `.rs` file lives under `<crate>/src/` but isn't
//!   reachable via any target's `mod` chain. Usually a renamed/moved module
//!   that left a stale source file behind.

use syn_workspace::{Crate, Module, Workspace};

use crate::diagnostic::Diagnostic;
use crate::diagnostic::builder::{at_file, at_line};
use crate::diagnostic::render::display_path;
use crate::lints::{Lint, LintContext, LintId, Requirements};

pub(crate) struct ModuleTree;

impl ModuleTree {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ModuleTree {
    fn default() -> Self {
        Self::new()
    }
}

impl Lint for ModuleTree {
    fn id(&self) -> LintId {
        LintId::ModuleTree
    }

    fn requirements(&self) -> Requirements {
        Requirements {
            needs_workspace: true,
        }
    }

    fn check(&self, cx: &LintContext<'_>) -> Vec<Diagnostic> {
        let workspace = cx
            .workspace
            .expect("module-tree lint requires Workspace (Requirements::needs_workspace)");
        check(workspace)
    }
}

pub(crate) fn check(workspace: &Workspace) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for krate in workspace.members() {
        for module in krate.all_modules() {
            collect_broken_mod_decls(module, &mut diagnostics);
        }
        collect_orphan_files(krate, &mut diagnostics);
    }
    diagnostics
}

fn collect_broken_mod_decls(module: &Module, out: &mut Vec<Diagnostic>) {
    let lint_id = LintId::ModuleTree.id();
    for decl in &module.broken_mod_decls {
        let msg = format!(
            "`mod {}` declared but no `{}.rs` or `{}/mod.rs` found",
            decl.name, decl.name, decl.name,
        );
        out.push(
            at_line(lint_id, msg, decl.declared_in.clone(), decl.line)
                .help(format!(
                    "create `{}.rs` adjacent to this file, or `{}/mod.rs`, or add a `#[path = \"…\"]` attribute",
                    decl.name, decl.name,
                ))
                .note("`mod foo;` with no inline body must resolve to a source file")
                .build(),
        );
    }
}

fn collect_orphan_files(krate: &Crate, out: &mut Vec<Diagnostic>) {
    let lint_id = LintId::ModuleTree.id();
    for path in &krate.orphan_files {
        let rel = path
            .strip_prefix(&krate.manifest_dir)
            .map(display_path)
            .unwrap_or_else(|_| display_path(path));

        out.push(
            at_file(lint_id, format!("orphan source file `{rel}` is not reachable from any `mod` declaration"), path.clone())
                .help(format!(
                    "add `mod {};` (or a `#[path = \"{}\"] mod ...;`) in the appropriate parent module, or delete the file",
                    path.file_stem().and_then(|s| s.to_str()).unwrap_or(""),
                    rel,
                ))
                .note(format!("crate `{}`'s module tree was built from `src/lib.rs` or `src/main.rs`", krate.name))
                .build(),
        );
    }
}
