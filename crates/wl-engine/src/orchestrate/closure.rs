//! Universe-correct package closures for scoped configs.
//!
//! A config declaring `-p app` must extract app **plus every workspace member
//! cargo compiles with it in that config's universe**, all as *primary* `-p`
//! units: only primary units get `CARGO_PRIMARY_PACKAGE`, which is what makes
//! dylint's driver file-dep the dylib so the relink force lever can refresh
//! their fragments when warm (a member compiled as a mere dependency keeps
//! its stale fragment forever).
//!
//! Correctness has three axes, all handled here:
//! - **platform** — the closure comes from a `--filter-platform <triple>`
//!   resolve for `--target` configs, so a host-only `[target.'cfg(unix)']`
//!   dep member is never passed as `-p` under wasm (a guaranteed compile
//!   failure);
//! - **kind** — normal + build edges always; dev edges only *from the
//!   declared roots* and only for a test-kind config (matching what
//!   `cargo test -p app` actually compiles);
//! - **features** — resolve graphs are feature-resolved with default
//!   features, and `cargo metadata` cannot re-resolve per-package feature
//!   selections; when the config declares features, optional member deps of
//!   in-closure members are *added* to the closure (over-approximation in
//!   the safe direction: a workspace member must compile standalone).

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use super::{ConfigSpec, EngineError, Kinds};
use cargo_metadata::DependencyKind;

/// One `cargo metadata` per distinct `--target` universe (the host universe
/// reuses the shared exec). `--filter-platform` drops dep edges whose
/// `[target.'cfg(…)']` condition can't hold on `triple`.
pub(super) fn universe_metadata(
    workspace_root: &std::path::Path,
    triple: &str,
) -> Result<cargo_metadata::Metadata, EngineError> {
    cargo_metadata::MetadataCommand::new()
        .manifest_path(workspace_root.join("Cargo.toml"))
        .other_options(vec!["--filter-platform".into(), triple.into()])
        .exec()
        .map_err(|source| EngineError::Metadata {
            dir: workspace_root.to_path_buf(),
            source: Box::new(source),
        })
}

/// Expand the spec's declared `-p` roots to the member closure cargo compiles
/// in this universe (package names, cargo form). Errors when a declared root
/// is not a workspace member — a silent skip would silently shrink the
/// extracted universe.
pub(super) fn member_closure(
    md: &cargo_metadata::Metadata,
    spec: &ConfigSpec,
) -> Result<Vec<String>, EngineError> {
    let members: BTreeSet<&str> = md
        .workspace_packages()
        .iter()
        .map(|p| p.name.as_str())
        .collect();
    let by_id: BTreeMap<&str, &cargo_metadata::Package> = md
        .packages
        .iter()
        .map(|p| (p.id.repr.as_str(), p))
        .collect();

    let mut roots: Vec<&cargo_metadata::Package> = Vec::new();
    for name in &spec.packages {
        if !members.contains(name.as_str()) {
            return Err(EngineError::UnknownPackage {
                package: name.clone(),
                config: spec.display.clone(),
            });
        }
        roots.extend(
            md.workspace_packages()
                .iter()
                .filter(|p| p.name.as_str() == name.as_str()),
        );
    }

    // BFS over the resolve graph. `resolve` is present (the universe metadata
    // is never run `--no-deps`); nodes carry per-edge dep kinds.
    let nodes: BTreeMap<&str, &cargo_metadata::Node> = md
        .resolve
        .as_ref()
        .map(|r| {
            r.nodes
                .iter()
                .map(|n| (n.id.repr.as_str(), n))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();

    let mut closure: BTreeSet<String> = BTreeSet::new();
    let mut queue: VecDeque<(&str, bool)> = roots
        .iter()
        .map(|p| (p.id.repr.as_str(), true)) // (id, is_declared_root)
        .collect();
    while let Some((id, is_root)) = queue.pop_front() {
        let Some(pkg) = by_id.get(id) else { continue };
        if !members.contains(pkg.name.as_str()) {
            continue; // externals aren't `-p`-able and never need forcing
        }
        if !closure.insert(pkg.name.to_string()) {
            continue;
        }
        let Some(node) = nodes.get(id) else { continue };
        for dep in &node.deps {
            let follow = dep.dep_kinds.iter().any(|k| {
                matches!(k.kind, DependencyKind::Normal | DependencyKind::Build)
                    || (k.kind == DependencyKind::Development
                        && is_root
                        && spec.kinds == Kinds::Tests)
            });
            if follow {
                queue.push_back((dep.pkg.repr.as_str(), false));
            }
        }
        // Feature axis: the resolve graph is default-features-resolved, so an
        // optional member dep a `--features` flag would enable is absent from
        // `node.deps`. Over-approximate from the manifest declaration.
        if !spec.features.is_default() {
            for d in &pkg.dependencies {
                if d.optional
                    && members.contains(d.name.as_str())
                    && let Some(dep_id) = member_id_by_name(md, &d.name)
                {
                    queue.push_back((dep_id, false));
                }
            }
        }
    }
    Ok(closure.into_iter().collect())
}

fn member_id_by_name<'a>(md: &'a cargo_metadata::Metadata, name: &str) -> Option<&'a str> {
    md.workspace_packages()
        .iter()
        .find(|p| p.name.as_str() == name)
        .map(|p| p.id.repr.as_str())
}
