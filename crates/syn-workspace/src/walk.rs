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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use cargo_metadata::{Message, MetadataCommand, Package, PackageId, TargetKind as CargoTargetKind};

use crate::manifest::Manifest;
use crate::resolve::{
    Crate, Error, LoadWarning, ResolvedPath, Result, Target, TargetKind, module_tree,
};
use module_tree::IncludeCtx;

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
    harvest_build_env: bool,
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

    let mut warnings: Vec<LoadWarning> = Vec::new();
    // Tier-2 generated-code support: harvest each relevant build-script crate's
    // runtime env (OUT_DIR + `cargo::rustc-env=` exports) so an
    // `include!(concat!(env!("OUT_DIR"), …))` resolves. Scoped to crates that
    // have BOTH a `build.rs` and an `include!` (the only crates whose includes
    // can need build env) and gated behind `harvest_build_env` — the offline
    // default skips it entirely. Any cargo failure degrades to an empty map, so
    // Tier-1 literal / `CARGO_*` includes still resolve.
    let build_env = if harvest_build_env {
        harvest_build_script_env(&root_manifest_path, &metadata, &mut warnings)
    } else {
        HashMap::new()
    };

    let mut out = Vec::new();
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

        // Per-crate `include!` environment: `CARGO_*` vars seeded from metadata
        // (offline, always present) overlaid with this crate's harvested
        // build-script env (present only under Tier-2, for build.rs∩include!
        // crates). Drives `env!(...)` const-folding during the module walk.
        let mut include_env: HashMap<String, String> = HashMap::new();
        include_env.insert(
            "CARGO_MANIFEST_DIR".to_string(),
            manifest_dir.to_string_lossy().into_owned(),
        );
        include_env.insert("CARGO_PKG_NAME".to_string(), pkg.name.to_string());
        include_env.insert("CARGO_PKG_VERSION".to_string(), pkg.version.to_string());
        if let Some(harvested) = build_env.get(&pkg.id) {
            for (k, v) in harvested {
                include_env.insert(k.clone(), v.clone());
            }
        }
        let inc = IncludeCtx {
            env: &include_env,
            depth: 0,
        };

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
            // the file itself here would wrongly resolve e.g. `tests/it.rs`'s
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
                inc,
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
        // Union of every target's spliced `include!` files (each module records
        // its own in `Module::generated_files`).
        let generated_files: Vec<PathBuf> = targets
            .iter()
            .flat_map(|t| t.all_modules())
            .flat_map(|m| m.generated_files.iter().cloned())
            .collect();

        let mut declared_features: Vec<String> = pkg.features.keys().cloned().collect();
        declared_features.sort();
        // Full activation lists (cargo synthesizes `foo = ["dep:foo"]` for an
        // implicit optional-dependency feature) so consumers can tell a
        // code-gating "leaf" feature (empty list) from a dependency/feature
        // "plumbing" one.
        let feature_values: std::collections::BTreeMap<String, Vec<String>> = pkg
            .features
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let manifest = Manifest::load(&manifest_path)?;

        out.push(Crate {
            name: pkg.name.to_string(),
            version: pkg.version.to_string(),
            manifest_dir,
            is_workspace_member: true,
            targets,
            orphan_files,
            generated_files,
            declared_features,
            feature_values,
            manifest,
        });
    }

    Ok((root_manifest, out, warnings))
}

/// Harvest the runtime environment of build-script crates so `include!`
/// arguments that reference `OUT_DIR` (or a custom `cargo::rustc-env=` var) can
/// be resolved. Runs `cargo check --message-format=json` scoped (`-p`) to the
/// crates that have BOTH a `build.rs` and an `include!` in their sources — the
/// only crates whose includes can need build-script env. Returns each such
/// crate's env keyed by package id (the `cargo::rustc-env=` exports plus the
/// synthesized `OUT_DIR`). Any failure (cargo missing, non-zero exit) is
/// recorded as a non-fatal [`LoadWarning`] and yields whatever it could parse —
/// Tier-1 literal / `CARGO_*` includes resolve without it regardless.
fn harvest_build_script_env(
    root_manifest_path: &Path,
    metadata: &cargo_metadata::Metadata,
    warnings: &mut Vec<LoadWarning>,
) -> HashMap<PackageId, HashMap<String, String>> {
    let pkgs: Vec<&Package> = metadata
        .workspace_packages()
        .into_iter()
        .filter(|pkg| has_build_script(pkg) && crate_mentions_include(pkg))
        .collect();
    if pkgs.is_empty() {
        return HashMap::new();
    }

    let mut cmd = std::process::Command::new("cargo");
    cmd.arg("check")
        .arg("--manifest-path")
        .arg(root_manifest_path)
        .arg("--message-format=json");
    for pkg in &pkgs {
        cmd.arg("-p").arg(pkg.name.to_string());
    }
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());

    let output = match cmd.output() {
        Ok(o) => o,
        Err(e) => {
            warnings.push(LoadWarning::BuildEnvHarvestFailed {
                message: format!("`cargo check` could not be run: {e}"),
            });
            return HashMap::new();
        }
    };
    if !output.status.success() {
        warnings.push(LoadWarning::BuildEnvHarvestFailed {
            message: format!(
                "`cargo check` exited with {} while harvesting build-script env",
                output.status
            ),
        });
        // Fall through: a build-script-executed message that already arrived is
        // still valid even if a later target's compile failed.
    }

    let mut map: HashMap<PackageId, HashMap<String, String>> = HashMap::new();
    for message in Message::parse_stream(output.stdout.as_slice()) {
        let Ok(Message::BuildScriptExecuted(bs)) = message else {
            continue;
        };
        let entry = map.entry(bs.package_id).or_default();
        for (k, v) in bs.env {
            entry.insert(k, v);
        }
        // `OUT_DIR` is reported as its own field, not inside `env`.
        entry.insert("OUT_DIR".to_string(), bs.out_dir.as_str().to_string());
    }
    map
}

/// True iff the package has a `build.rs` (a custom-build cargo target).
fn has_build_script(pkg: &Package) -> bool {
    pkg.targets.iter().any(|t| {
        t.kind
            .iter()
            .any(|k| matches!(k, CargoTargetKind::CustomBuild))
    })
}

/// Cheap text pre-scan: does any `.rs` file in the crate mention `include!`?
/// Lets the harvest skip a `cargo check` for a build-script crate that has no
/// `include!` to resolve. A substring false positive only costs a spurious
/// check; a parse-free match is robust enough since `std::include!` contains it.
fn crate_mentions_include(pkg: &Package) -> bool {
    let Some(dir) = pkg.manifest_path.as_std_path().parent() else {
        return false;
    };
    // Scan raw bytes (no UTF-8 validation, no `String` allocation) for the literal
    // `include!` token; `windows` is O(n) and short-circuits on the first hit.
    const NEEDLE: &[u8] = b"include!";
    rs_files_under(dir).iter().any(|f| {
        std::fs::read(f).is_ok_and(|bytes| bytes.windows(NEEDLE.len()).any(|w| w == NEEDLE))
    })
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
            // A file spliced in via `include!(...)` is reached even though it is
            // no module's own `file` (its items live in the including module).
            for gen_file in &module.generated_files {
                if let Ok(canon) = gen_file.canonicalize() {
                    reached.insert(canon);
                } else {
                    reached.insert(gen_file.clone());
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
