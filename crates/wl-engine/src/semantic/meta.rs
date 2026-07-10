//! Workspace facts from `cargo metadata` (manifest parsing only — no compile).
//!
//! Two verdicts read this one exec: the unused-pub **boundary** (publish /
//! target-kind roots, SPIKE §7 step 5) and the **declared dependency tables**
//! (the unused-deps substrate). Lifted from the spike assembler's `Meta`.

use std::collections::{BTreeMap, BTreeSet};

/// Which cargo dependency table a declared dep came from — the axis that
/// decides whether the IR can judge it. `Normal` deps compile in every lib/bin
/// build; `Dev` deps only when a test/example/bench target was compiled;
/// `Build` deps drive `build.rs`, which isn't lint-passed, so they're never
/// judged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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
    /// Platform-gated (declared under `[target.<cfg>.…]`): it only compiles
    /// when the cfg matches the build host, so a config run on the wrong host
    /// can't observe usage. Never flagged (skipped), like `optional`.
    pub target_gated: bool,
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
    /// Package code-name → its lib target's code-name, for every package in
    /// the resolve graph whose lib target is named differently (`md-5`'s lib
    /// is `md5`). Reference edges carry the *lib crate* name (`tcx.crate_name`),
    /// while declared deps and the resolve graph speak *package* names — this
    /// map bridges the two in [`WorkspaceMeta::dep_closure`].
    pub(super) lib_name_of: BTreeMap<String, String>,
}

impl WorkspaceMeta {
    /// Build the workspace facts from an already-read `cargo metadata` (with
    /// `resolve`, so the dependency graph is available for facade-crate
    /// attribution). The engine reads metadata once during extraction and
    /// threads it here via [`ExtractionRuns`], so the assembler never re-execs
    /// cargo.
    ///
    /// [`ExtractionRuns`]: crate::ExtractionRuns
    pub fn from_metadata(md: &cargo_metadata::Metadata) -> Self {
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

        // Lib-target names, over ALL packages (deps included): reference edges
        // carry `tcx.crate_name` — the lib target's name — which differs from
        // the package name for crates like `md-5` (lib `md5`).
        let mut lib_name_of = BTreeMap::new();
        for p in &md.packages {
            let pkg = p.name.to_string().replace('-', "_");
            for t in &p.targets {
                if t.kind.iter().any(|k| {
                    matches!(
                        k.to_string().as_str(),
                        "lib" | "rlib" | "dylib" | "cdylib" | "staticlib" | "proc-macro"
                    )
                }) {
                    let lib = t.name.replace('-', "_");
                    if lib != pkg {
                        lib_name_of.insert(pkg.clone(), lib);
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
            lib_name_of,
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
                // Every member's build script is crate `build_script_build`
                // (the target name is `build-script-build`): mapping it into
                // `target_owner` would hand ALL build fragments to whichever
                // member iterated last, crediting build.rs references to that
                // member's `[dependencies]` and masking true unused-deps
                // findings. Build fragments deliberately have no owner here —
                // `DepUsage::compute` skips them (build-deps stay unjudged).
                if t.kind.iter().any(|k| k.to_string() == "custom-build") {
                    continue;
                }
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
                        _ => {} // bin — owner-mapped above, nothing to classify
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
                    target_gated: d.target.is_some(),
                })
                .collect();
            meta.declared.insert(pkg, decls);
        }
        meta
    }

    pub fn is_published_lib(&self, krate: &str) -> bool {
        self.published_libs.contains(krate)
    }

    pub fn members(&self) -> impl Iterator<Item = &str> {
        self.members.iter().map(String::as_str)
    }

    /// The resolved dependency **closure** of a crate — the crate itself plus
    /// every crate reachable from it in the resolve graph, each contributed
    /// under BOTH its package name and its lib-target name (edges carry the
    /// lib name; see [`Self::lib_name_of`]). A declared dep is "exercised" if
    /// the referenced-crate set intersects this: a reference to `clap_builder`
    /// counts as using the declared facade `clap`, because
    /// `clap_builder ∈ closure(clap)`; a reference to `md5` counts as using
    /// the declared `md-5`, its owning package.
    pub(super) fn dep_closure(&self, name: &str) -> BTreeSet<String> {
        let mut seen = BTreeSet::new();
        let mut stack = vec![name.to_string()];
        while let Some(n) = stack.pop() {
            if !seen.insert(n.clone()) {
                continue;
            }
            if let Some(lib) = self.lib_name_of.get(&n) {
                seen.insert(lib.clone());
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
                            target_gated: false,
                        },
                        DepDecl {
                            name: "never_used".into(),
                            kind: DepKind::Normal,
                            optional: false,
                            target_gated: false,
                        },
                        DepDecl {
                            name: "dev_helper".into(),
                            kind: DepKind::Dev,
                            optional: false,
                            target_gated: false,
                        },
                        DepDecl {
                            name: "hook_installer".into(),
                            kind: DepKind::Build,
                            optional: false,
                            target_gated: false,
                        },
                        DepDecl {
                            name: "feature_gated".into(),
                            kind: DepKind::Normal,
                            optional: true,
                            target_gated: false,
                        },
                        DepDecl {
                            name: "md_5".into(),
                            kind: DepKind::Normal,
                            optional: false,
                            target_gated: false,
                        },
                        DepDecl {
                            name: "platform_only".into(),
                            kind: DepKind::Normal,
                            optional: false,
                            target_gated: true,
                        },
                    ],
                ),
                ("beta".into(), Vec::new()),
            ]
            .into(),
            pkg_deps: [("facade".into(), ["facade_core".into()].into())].into(),
            // Package `md-5` exposes lib `md5` — edges carry the lib name.
            lib_name_of: [("md_5".into(), "md5".into())].into(),
        }
    }
}
