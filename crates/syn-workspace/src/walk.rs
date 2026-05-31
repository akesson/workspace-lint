//! Workspace discovery — turn a directory into a list of [`Crate`] entries.
//!
//! Uses `cargo_metadata` to invoke `cargo metadata` against the workspace
//! root, then materializes each workspace package's targets (lib, bin,
//! example, test, bench, build script, proc-macro library) as
//! [`Target`] entries with their own module trees.
//!
//! Each target's tree is built using the *parent crate's* code-form name
//! as the canonical root. That keeps cross-crate references (e.g.
//! `serde::Foo` inside an integration test) attributed to the parent
//! crate's reference set; intra-target paths like `crate::helpers::foo`
//! become `parent_crate::helpers::foo` and get filtered as self-references
//! at consumer level.
//!
//! Orphan `.rs` files (under `src/`, not reached by any target's module
//! tree and not the `src_path` of any other target) are computed per crate
//! and attached to [`Crate::orphan_files`].
//!
//! External crates (transitive cargo deps) are not yet materialized — the
//! list is just workspace members. The [`Crate`] model carries
//! `is_workspace_member` so external crates can be added later without an
//! API change.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use cargo_metadata::{MetadataCommand, TargetKind as CargoTargetKind};

use crate::manifest::Manifest;
use crate::resolve::{
    Crate, Error, LoadWarning, ResolvedPath, Result, Target, TargetKind, module_tree,
};

/// Run `cargo metadata` on the workspace at `root` and return the absolute
/// path of every workspace member's `Cargo.toml`.
///
/// Honors cargo's full workspace semantics: `members`, glob patterns,
/// `exclude`, and `default-members`. Use this in preference to parsing the
/// root `Cargo.toml`'s `members` table by hand — the by-hand version
/// silently diverges on `exclude` and non-trivial globs.
pub fn member_manifests(root: &Path) -> Result<Vec<PathBuf>> {
    let manifest = root.join("Cargo.toml");
    // `--no-deps` for the same reason as `load_members`: only workspace member
    // manifests are needed, so dependency resolution (and the network / lockfile
    // it would require) is pure overhead.
    let metadata = MetadataCommand::new()
        .manifest_path(&manifest)
        .no_deps()
        .exec()?;
    Ok(metadata
        .workspace_packages()
        .into_iter()
        .map(|pkg| pkg.manifest_path.as_std_path().to_path_buf())
        .collect())
}

/// Run `cargo metadata` on the workspace at `root` and return the root
/// [`Manifest`] alongside one [`Crate`] per workspace member, plus any
/// non-fatal warnings collected during the walk.
///
/// `marker_crates` is forwarded through the module-tree pipeline so the
/// `expansion_uses!` annotation can match against caller-configured
/// crate names (see [`crate::LoadOptions::marker_crates`]).
pub(crate) fn load_members(
    root: &Path,
    marker_crates: &[String],
) -> Result<(Manifest, Vec<Crate>, Vec<LoadWarning>)> {
    let root_manifest_path = root.join("Cargo.toml");
    // `--no-deps`: we materialize only workspace members (see the
    // `workspace_packages()` loop below) and represent external crates by name
    // alone, so cargo's dependency *resolution* is pure overhead — and it would
    // force a resolvable graph (network / a populated registry / a lockfile)
    // just to load. Skipping it makes `Workspace::load` work offline on any
    // crate, which the Phase-2 corpus relies on.
    let metadata = MetadataCommand::new()
        .manifest_path(&root_manifest_path)
        .no_deps()
        .exec()?;

    let root_manifest = Manifest::load(&root_manifest_path)?;

    let mut out = Vec::new();
    let mut warnings: Vec<LoadWarning> = Vec::new();
    for pkg in metadata.workspace_packages() {
        let manifest_path = pkg.manifest_path.as_std_path().to_path_buf();
        let manifest_dir = manifest_path
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| {
                Error::manifest(
                    &manifest_path,
                    std::io::Error::other("manifest path has no parent"),
                )
            })?;

        // Cargo crate names use hyphens (e.g. `data-models`), but source code
        // references them with underscores (`use data_models::...`). The
        // resolver stores canonical paths in code form so they line up with
        // what bindings see; `Crate.name` keeps the cargo form so user-facing
        // diagnostics and `from`-pattern matching see the same string users
        // wrote in `Cargo.toml`.
        let code_name = pkg.name.replace('-', "_");

        let mut targets = Vec::new();
        for cargo_target in &pkg.targets {
            let Some(kind) = pick_target_kind(&cargo_target.kind) else {
                continue;
            };
            let src_path = cargo_target.src_path.as_std_path().to_path_buf();
            if !src_path.exists() {
                continue;
            }
            // Each target gets its own module tree, rooted at its
            // `src_path` and using the parent crate's code_name as the
            // canonical root. Build failures on auxiliary targets
            // (test/example) shouldn't crash the whole resolver — they
            // get recorded as `LoadWarning::TargetParseFailed` so the
            // caller can decide what to do. For lib/bin we propagate.
            let canonical = ResolvedPath::new([code_name.clone()]);
            // A target root (lib/bin/example/test/bench/build-script) is a crate
            // boundary that owns its containing directory, regardless of its
            // filename — its `mod foo;` children resolve as siblings. (Passing
            // the file itself here would mis-resolve e.g. `tests/it.rs`'s
            // `mod common;` into `tests/it/`.)
            let mod_dir = src_path.parent().unwrap_or(std::path::Path::new("."));
            let root_module = match module_tree::build_module_from_file(
                &src_path,
                mod_dir,
                code_name.clone(),
                canonical,
                // The target's root module IS the crate boundary, not a
                // `mod foo;` declaration — Public by definition.
                crate::resolve::Visibility::Public,
                marker_crates,
            ) {
                Ok(m) => m,
                Err(e) => {
                    if matches!(
                        kind,
                        TargetKind::Lib | TargetKind::ProcMacro | TargetKind::Bin
                    ) {
                        return Err(e);
                    }
                    warnings.push(LoadWarning::TargetParseFailed {
                        target: cargo_target.name.clone(),
                        path: src_path.clone(),
                        message: e.to_string(),
                    });
                    continue;
                }
            };

            targets.push(Target {
                kind,
                name: cargo_target.name.clone(),
                src_path,
                root: root_module,
            });
        }

        let orphan_files = compute_orphans(&manifest_dir, &targets);

        let mut declared_features: Vec<String> = pkg.features.keys().cloned().collect();
        declared_features.sort();

        let manifest = Manifest::load(&manifest_path)?;

        out.push(Crate {
            name: pkg.name.to_string(),
            version: pkg.version.to_string(),
            manifest_dir,
            is_workspace_member: true,
            targets,
            orphan_files,
            declared_features,
            manifest,
        });
    }

    Ok((root_manifest, out, warnings))
}

/// Map cargo's per-target `kind: Vec<TargetKind>` (which may report
/// multiple crate-types for one target — e.g. `["lib", "cdylib"]`) onto
/// our coalesced [`TargetKind`]. `ProcMacro` outranks `Lib`; everything
/// else falls through in priority order. Unknown kinds yield `None`,
/// causing the target to be silently dropped.
fn pick_target_kind(kinds: &[CargoTargetKind]) -> Option<TargetKind> {
    if kinds
        .iter()
        .any(|k| matches!(k, CargoTargetKind::ProcMacro))
    {
        return Some(TargetKind::ProcMacro);
    }
    if kinds.iter().any(|k| {
        matches!(
            k,
            CargoTargetKind::Lib
                | CargoTargetKind::RLib
                | CargoTargetKind::DyLib
                | CargoTargetKind::CDyLib
                | CargoTargetKind::StaticLib
        )
    }) {
        return Some(TargetKind::Lib);
    }
    for k in kinds {
        match k {
            CargoTargetKind::Bin => return Some(TargetKind::Bin),
            CargoTargetKind::Example => return Some(TargetKind::Example),
            CargoTargetKind::Test => return Some(TargetKind::Test),
            CargoTargetKind::Bench => return Some(TargetKind::Bench),
            CargoTargetKind::CustomBuild => return Some(TargetKind::BuildScript),
            _ => {}
        }
    }
    None
}

/// `.rs` files under `<manifest_dir>/src/` that aren't reached by any
/// target's module tree and aren't the `src_path` of any target.
fn compute_orphans(manifest_dir: &Path, targets: &[Target]) -> Vec<PathBuf> {
    let src_dir = manifest_dir.join("src");
    if !src_dir.is_dir() {
        return Vec::new();
    }

    // Files reached by any target's module tree, plus each target's
    // top-level src_path. Canonicalize so symlinks compare equal.
    let mut reached: HashSet<PathBuf> = HashSet::new();
    for target in targets {
        if let Ok(canon) = target.src_path.canonicalize() {
            reached.insert(canon);
        } else {
            reached.insert(target.src_path.clone());
        }
        for module in target.all_modules() {
            if let Some(file) = &module.file {
                if let Ok(canon) = file.canonicalize() {
                    reached.insert(canon);
                } else {
                    reached.insert(file.clone());
                }
            }
        }
    }

    let mut orphans = Vec::new();
    for path in rs_files_under(&src_dir) {
        let canon = path.canonicalize().unwrap_or_else(|_| path.clone());
        if !reached.contains(&canon) && !reached.contains(&path) {
            orphans.push(path);
        }
    }
    orphans
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
