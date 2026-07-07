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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrate::command::parse_command;

    /// Hand-built `cargo metadata` JSON: `members` become workspace packages,
    /// `externals` registry packages; `edges` is `(from, to, kind)` with kind
    /// one of `"normal"` / `"dev"` / `"build"`. `optionals` adds a manifest
    /// declaration (resolve edge absent — exactly cargo's shape for an
    /// optional dep the default resolve didn't enable).
    fn metadata(
        members: &[&str],
        externals: &[&str],
        edges: &[(&str, &str, &str)],
        optionals: &[(&str, &str)],
    ) -> cargo_metadata::Metadata {
        let id = |name: &str| format!("registry+https://crates.io/{name}#0.1.0");
        let pkg = |name: &str, member: bool| {
            let deps: Vec<_> = optionals
                .iter()
                .filter(|(f, _)| *f == name)
                .map(|(_, to)| {
                    serde_json::json!({
                        "name": to, "req": "*", "kind": null, "optional": true,
                        "uses_default_features": true, "features": [],
                        "source": null, "target": null, "rename": null,
                        "registry": null,
                    })
                })
                .collect();
            serde_json::json!({
                "name": name, "version": "0.1.0", "id": id(name),
                "license": null, "license_file": null, "description": null,
                "source": if member { serde_json::Value::Null } else { "registry".into() },
                "dependencies": deps, "targets": [], "features": {},
                "manifest_path": format!("/w/{name}/Cargo.toml"),
                "metadata": null, "publish": null, "authors": [],
                "categories": [], "keywords": [], "readme": null,
                "repository": null, "homepage": null, "documentation": null,
                "edition": "2024", "links": null, "default_run": null,
                "rust_version": null,
            })
        };
        let node = |name: &str| {
            let deps: Vec<_> = edges
                .iter()
                .filter(|(from, _, _)| *from == name)
                .map(|(_, to, kind)| {
                    serde_json::json!({
                        "name": to.replace('-', "_"), "pkg": id(to),
                        "dep_kinds": [{
                            "kind": if *kind == "normal" { serde_json::Value::Null } else { (*kind).into() },
                            "target": null,
                        }],
                    })
                })
                .collect();
            serde_json::json!({
                "id": id(name), "deps": deps, "dependencies": [], "features": [],
            })
        };
        let all: Vec<&str> = members.iter().chain(externals).copied().collect();
        serde_json::from_value(serde_json::json!({
            "packages": all.iter().map(|n| pkg(n, members.contains(n))).collect::<Vec<_>>(),
            "workspace_members": members.iter().map(|n| id(n)).collect::<Vec<_>>(),
            "workspace_default_members": members.iter().map(|n| id(n)).collect::<Vec<_>>(),
            "resolve": {
                "nodes": all.iter().map(|n| node(n)).collect::<Vec<_>>(),
                "root": null,
            },
            "workspace_root": "/w", "target_directory": "/w/target",
            "version": 1, "metadata": null,
        }))
        .expect("synthetic metadata deserializes")
    }

    fn closure(md: &cargo_metadata::Metadata, cmd: &str) -> Vec<String> {
        member_closure(md, &parse_command(cmd).unwrap()).unwrap()
    }

    #[test]
    fn roots_expand_over_normal_edges_members_only() {
        let md = metadata(
            &["app", "lib", "other"],
            &["serde"],
            &[("app", "lib", "normal"), ("app", "serde", "normal")],
            &[],
        );
        // `lib` joins (member, normal edge); `serde` (external) and `other`
        // (member, unreferenced) don't.
        assert_eq!(closure(&md, "cargo build -p app"), ["app", "lib"]);
    }

    #[test]
    fn dev_edges_only_from_roots_and_only_for_tests() {
        let md = metadata(
            &["app", "lib", "devlib", "deep"],
            &[],
            &[
                ("app", "lib", "normal"),
                ("app", "devlib", "dev"),
                ("lib", "deep", "dev"),
            ],
            &[],
        );
        // Default kind: no dev edges at all.
        assert_eq!(closure(&md, "cargo build -p app"), ["app", "lib"]);
        // Tests kind: the root's dev deps compile, a transitive member's don't
        // (`cargo test -p app` doesn't build lib's dev-deps).
        assert_eq!(closure(&md, "cargo test -p app"), ["app", "devlib", "lib"]);
    }

    #[test]
    fn build_edges_always_follow() {
        let md = metadata(
            &["app", "buildlib"],
            &[],
            &[("app", "buildlib", "build")],
            &[],
        );
        assert_eq!(closure(&md, "cargo build -p app"), ["app", "buildlib"]);
    }

    #[test]
    fn unknown_root_is_an_error_naming_the_config() {
        let md = metadata(&["app"], &[], &[], &[]);
        let spec = parse_command("cargo build -p nope").unwrap();
        match member_closure(&md, &spec) {
            Err(EngineError::UnknownPackage { package, config }) => {
                assert_eq!(package, "nope");
                assert_eq!(config, "cargo build -p nope");
            }
            other => panic!("expected UnknownPackage, got {other:?}"),
        }
    }

    #[test]
    fn declared_features_pull_in_optional_member_deps() {
        // The resolve graph (default features) has no app→opt edge; only the
        // manifest declares it, optional. A `--features` config must
        // over-approximate it into the closure.
        let md = metadata(&["app", "opt"], &[], &[], &[("app", "opt")]);
        assert_eq!(closure(&md, "cargo build -p app"), ["app"]);
        assert_eq!(
            closure(&md, "cargo build -p app --features extras"),
            ["app", "opt"]
        );
    }
}
