//! Module-tree integrity — orphan files and dangling `mod` declarations.
//!
//! Two structural failure modes flagged here:
//!
//! - **mod_decl_missing_target**: `mod foo;` appears in source but neither
//!   `foo.rs` nor `foo/mod.rs` exists adjacent to the parent file, and no
//!   `#[path = "..."]` override resolves either. Recorded by syn-workspace's
//!   [`BrokenModDecl`] during the Tier 2 walk; the check just reports them.
//! - **orphan_rs_file**: a `.rs` file lives under `<crate>/src/` but isn't
//!   reachable via any target's `mod` chain (and isn't the `src_path` of
//!   some other target). Usually a renamed/moved module that left a stale
//!   source file behind.
//!
//! Both data sources are pre-computed by the resolver: `BrokenModDecl`
//! entries hang off each `Module`, and orphan files are computed
//! per-crate during workspace load and stored on `Crate::orphan_files`.

use syn_workspace::{Crate, Module, Workspace};

use crate::diagnostic::Diagnostic;
use crate::diagnostic::builder::{at_file, at_line};
use crate::diagnostic::render::display_path;

pub const LINT: &str = crate::lints::LintId::ModuleTree.id();

pub fn check(workspace: &Workspace) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for krate in workspace.crates() {
        if !krate.is_workspace_member {
            continue;
        }
        for module in krate.all_modules() {
            collect_broken_mod_decls(module, &mut diagnostics);
        }
        collect_orphan_files(krate, &mut diagnostics);
    }
    diagnostics
}

fn collect_broken_mod_decls(module: &Module, out: &mut Vec<Diagnostic>) {
    for decl in &module.broken_mod_decls {
        let msg = format!(
            "`mod {}` declared but no `{}.rs` or `{}/mod.rs` found",
            decl.name, decl.name, decl.name,
        );
        out.push(
            at_line(LINT, msg, decl.declared_in.clone(), decl.line)
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
    for path in &krate.orphan_files {
        let rel = path
            .strip_prefix(&krate.manifest_dir)
            .map(display_path)
            .unwrap_or_else(|_| display_path(path));

        out.push(
            at_file(LINT, format!("orphan source file `{rel}` is not reachable from any `mod` declaration"), path.clone())
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
