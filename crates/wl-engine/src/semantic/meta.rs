//! Workspace facts from `cargo metadata` (manifest parsing only — no compile).
//!
//! Two verdicts read this one exec: the unused-pub **boundary** (publish /
//! target-kind roots, SPIKE §7 step 5) and the **declared dependency tables**
//! (the unused-deps substrate). Lifted from the spike assembler's `Meta`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::SemanticError;

/// Which cargo dependency table a declared dep came from — the axis that
/// decides whether the IR can judge it. `Normal` deps compile in every lib/bin
/// build; `Dev` deps only when a test/example/bench target was compiled;
/// `Build` deps drive `build.rs`, which isn't lint-passed, so they're never
/// judged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepKind {
    Normal,
    Dev,
    Build,
}

/// One declared dependency of a workspace member, as `cargo metadata` reports
/// it — the `unused-deps` unit of judgement.
#[derive(Debug, Clone)]
pub struct DepDecl {
    /// The dependency crate's **package** name in code form (`-`→`_`). This is
    /// exactly what an edge target's crate segment (`RefEdge::to[0]`) carries —
    /// even under a `package = "…"` rename, because a crate's `tcx.crate_name`
    /// is its real name, not the local alias — so it joins directly against
    /// the exercised set with no rename bookkeeping.
    pub name: String,
    pub kind: DepKind,
    /// Feature-gated (`optional = true`): the crate isn't compiled unless its
    /// feature is on, so a config that didn't enable it can't observe usage.
    /// Never flagged (skipped), to avoid calling a feature-gated dep dead.
    pub optional: bool,
}

/// Workspace facts the semantic verdicts need, from one
/// `cargo metadata` exec (with `resolve`, for facade-crate closures).
pub struct WorkspaceMeta {
    /// Every workspace member's code-form package name.
    pub(super) members: BTreeSet<String>,
    /// Members that are publishable libraries — their pub API is an external
    /// reachability root (the unused-pub boundary). A pub item can only have
    /// out-of-workspace consumers if its crate is a publishable library; a
    /// bin's pub API and a `publish = false` lib's have no external root, so
    /// unused = dead.
    pub(super) published_libs: BTreeSet<String>,
    /// Fragment crate-name (code form) → the member **package** that owns that
    /// target. A package has many targets (lib, bin, each integration test),
    /// and a dep declared once at the package level may be used by any of
    /// them — usage folds onto the owning package before judging.
    pub(super) target_owner: BTreeMap<String, String>,
    /// Crate-names of test/example/bench targets. Dev-deps are judgeable only
    /// when one of these was actually compiled (a `--tests` config present).
    pub(super) test_targets: BTreeSet<String>,
    /// Member package → its declared dependencies.
    pub(super) declared: BTreeMap<String, Vec<DepDecl>>,
    /// Resolved dependency graph, code-name → direct dependency code-names.
    /// The fix for **facade crates**: a declared dep like `clap` re-exports
    /// everything from `clap_builder`, so references resolve to the *defining*
    /// crate, never the facade. Crediting a dep when any crate in its resolved
    /// closure is referenced clears that false positive. Empty if `resolve` is
    /// absent (degrades to exact-name matching).
    pub(super) pkg_deps: BTreeMap<String, BTreeSet<String>>,
}

impl WorkspaceMeta {
    /// Read the target workspace via `cargo metadata` (WITH `resolve`, so the
    /// dependency graph is available for facade-crate attribution).
    pub fn from_workspace(root: &Path) -> Result<Self, SemanticError> {
        let md = cargo_metadata::MetadataCommand::new()
            .manifest_path(root.join("Cargo.toml"))
            .exec()
            .map_err(|source| SemanticError::Metadata {
                dir: root.to_path_buf(),
                source: Box::new(source),
            })?;

        let member_ids: BTreeSet<String> = md
            .workspace_members
            .iter()
            .map(|id| id.to_string())
            .collect();
        // Package id → code-form crate name, over ALL packages (dep graph).
        let id_name: BTreeMap<String, String> = md
            .packages
            .iter()
            .map(|p| (p.id.to_string(), p.name.to_string().replace('-', "_")))
            .collect();
        // Resolved dep edges by code-name (all kinds — an over-approx closure
        // only ever over-credits, i.e. risks a false negative, never a false
        // positive — the safe direction for a "delete it" lint).
        let mut pkg_deps: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        if let Some(resolve) = &md.resolve {
            for node in &resolve.nodes {
                let Some(name) = id_name.get(&node.id.to_string()) else {
                    continue;
                };
                let entry = pkg_deps.entry(name.clone()).or_default();
                for dep_id in &node.dependencies {
                    if let Some(dn) = id_name.get(&dep_id.to_string()) {
                        entry.insert(dn.clone());
                    }
                }
            }
        }

        let mut meta = Self {
            members: BTreeSet::new(),
            published_libs: BTreeSet::new(),
            target_owner: BTreeMap::new(),
            test_targets: BTreeSet::new(),
            declared: BTreeMap::new(),
            pkg_deps,
        };
        for p in &md.packages {
            if !member_ids.contains(&p.id.to_string()) {
                continue; // a transitive dependency, not a workspace member
            }
            let pkg = p.name.to_string().replace('-', "_");
            meta.members.insert(pkg.clone());

            // `publish`: None ⇒ any registry; Some([]) ⇒ `publish = false`.
            let publishable = p.publish.as_ref().map(|v| !v.is_empty()).unwrap_or(true);
            let mut has_lib = false;
            for t in &p.targets {
                let tname = t.name.replace('-', "_");
                meta.target_owner.insert(tname.clone(), pkg.clone());
                // Compare kinds via Display so we don't couple to
                // cargo_metadata's enum representation. A target may carry
                // several kinds; classify by any that matters.
                for k in &t.kind {
                    match k.to_string().as_str() {
                        "lib" | "rlib" | "dylib" | "cdylib" | "staticlib" | "proc-macro" => {
                            has_lib = true;
                        }
                        "test" | "example" | "bench" => {
                            meta.test_targets.insert(tname.clone());
                        }
                        _ => {} // bin, custom-build (build.rs — not lint-passed)
                    }
                }
            }
            if publishable && has_lib {
                meta.published_libs.insert(pkg.clone());
            }

            let decls = p
                .dependencies
                .iter()
                .map(|d| DepDecl {
                    name: d.name.replace('-', "_"),
                    kind: match d.kind {
                        cargo_metadata::DependencyKind::Development => DepKind::Dev,
                        cargo_metadata::DependencyKind::Build => DepKind::Build,
                        _ => DepKind::Normal, // Normal + any future/unknown kind
                    },
                    optional: d.optional,
                })
                .collect();
            meta.declared.insert(pkg, decls);
        }
        Ok(meta)
    }

    pub fn is_published_lib(&self, krate: &str) -> bool {
        self.published_libs.contains(krate)
    }

    pub fn members(&self) -> impl Iterator<Item = &str> {
        self.members.iter().map(String::as_str)
    }

    /// The resolved dependency **closure** of a crate — the crate itself plus
    /// every crate reachable from it in the resolve graph. A declared dep is
    /// "exercised" if the referenced-crate set intersects this: a reference to
    /// `clap_builder` counts as using the declared facade `clap`, because
    /// `clap_builder ∈ closure(clap)`.
    pub(super) fn dep_closure(&self, name: &str) -> BTreeSet<String> {
        let mut seen = BTreeSet::new();
        let mut stack = vec![name.to_string()];
        while let Some(n) = stack.pop() {
            if !seen.insert(n.clone()) {
                continue;
            }
            if let Some(deps) = self.pkg_deps.get(&n) {
                stack.extend(deps.iter().cloned());
            }
        }
        seen
    }
}

#[cfg(test)]
pub(super) mod test_support {
    use super::*;

    /// A hand-built meta for the golden fixtures: two members (`alpha` a
    /// published lib, `beta` a bin), `alpha` declaring the facade dep `facade`
    /// whose closure contains `facade_core`.
    pub(crate) fn fixture_meta() -> WorkspaceMeta {
        WorkspaceMeta {
            members: ["alpha".into(), "beta".into()].into(),
            published_libs: ["alpha".into()].into(),
            target_owner: [
                ("alpha".into(), "alpha".into()),
                ("beta".into(), "beta".into()),
                ("alpha_it".into(), "alpha".into()), // integration test target
            ]
            .into(),
            test_targets: ["alpha_it".into()].into(),
            declared: [
                (
                    "alpha".into(),
                    vec![
                        DepDecl {
                            name: "facade".into(),
                            kind: DepKind::Normal,
                            optional: false,
                        },
                        DepDecl {
                            name: "never_used".into(),
                            kind: DepKind::Normal,
                            optional: false,
                        },
                        DepDecl {
                            name: "dev_helper".into(),
                            kind: DepKind::Dev,
                            optional: false,
                        },
                        DepDecl {
                            name: "hook_installer".into(),
                            kind: DepKind::Build,
                            optional: false,
                        },
                        DepDecl {
                            name: "feature_gated".into(),
                            kind: DepKind::Normal,
                            optional: true,
                        },
                    ],
                ),
                ("beta".into(), Vec::new()),
            ]
            .into(),
            pkg_deps: [("facade".into(), ["facade_core".into()].into())].into(),
        }
    }
}
