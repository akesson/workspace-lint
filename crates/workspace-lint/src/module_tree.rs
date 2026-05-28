//! Module-tree integrity — orphan files and dangling `mod` declarations.
//!
//! Two structural failure modes flagged here:
//!
//! - **mod_decl_missing_target**: `mod foo;` appears in source but neither
//!   `foo.rs` nor `foo/mod.rs` exists adjacent to the parent file, and no
//!   `#[path = "..."]` override resolves either. Recorded by syn-workspace's
//!   [`BrokenModDecl`] during the Tier 2 walk; the check just reports them.
//! - **orphan_rs_file**: a `.rs` file lives under `<crate>/src/` but isn't
//!   reachable via any `mod` chain. Usually a renamed/moved module that
//!   left a stale source file behind.
//!
//! The check walks the resolver's module tree to collect the set of
//! reachable files, then walks the filesystem to enumerate every `.rs` under
//! `src/`. The diff is the orphan set.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

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
        collect_broken_mod_decls(&krate.root, &mut diagnostics);
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
    for sub in &module.submodules {
        collect_broken_mod_decls(sub, out);
    }
}

fn collect_orphan_files(krate: &Crate, out: &mut Vec<Diagnostic>) {
    let src_dir = krate.manifest_dir.join("src");
    if !src_dir.is_dir() {
        return;
    }

    let mut reachable: HashSet<PathBuf> = HashSet::new();
    collect_reachable_files(&krate.root, &mut reachable);

    for path in rs_files_under(&src_dir) {
        let canon = path.canonicalize().unwrap_or(path.clone());
        let in_reachable = reachable.iter().any(|r| {
            let r_canon = r.canonicalize().unwrap_or_else(|_| r.clone());
            r_canon == canon || r == &path
        });
        if in_reachable {
            continue;
        }
        // Forward-slash normalize so the message body matches the renderer's
        // header on every platform (Windows otherwise emits `src\orphan.rs`
        // inside the message while the `--> ...` header is already `src/...`).
        let rel = path
            .strip_prefix(&krate.manifest_dir)
            .map(display_path)
            .unwrap_or_else(|_| display_path(&path));

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

fn collect_reachable_files(module: &Module, out: &mut HashSet<PathBuf>) {
    if let Some(f) = &module.file {
        out.insert(f.clone());
    }
    for sub in &module.submodules {
        collect_reachable_files(sub, out);
    }
}

/// Recursively list `.rs` files under `dir`, excluding `target/` and
/// hidden directories.
fn rs_files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let read = match std::fs::read_dir(&current) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for entry in read.flatten() {
            let path = entry.path();
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            if name.starts_with('.') || name == "target" {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }
    out
}
