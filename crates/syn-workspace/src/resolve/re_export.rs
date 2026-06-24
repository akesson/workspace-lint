//! Tier 2.5: `pub use` re-export chain following.
//!
//! Once Tiers 1 and 2 have produced per-file scopes and module trees, this
//! tier builds a graph of `pub use` edges and computes the canonical
//! definition for every re-exported name.
//!
//! Example chain:
//!
//! ```ignore
//! // in crate `data-models`
//! pub mod internal { pub struct User; }
//! pub use internal::User;             // edge: data_models::User -> data_models::internal::User
//!
//! // in crate `data-api`
//! pub use data_models::User;          // edge: data_api::User -> data_models::User
//! ```
//!
//! [`ReExportIndex::canonical`] chases edges until it reaches a non-`pub use`
//! item, returning the canonical [`ResolvedPath`]. Cycles (`pub use self::X`
//! and friends) are detected and the chain is broken at the first revisit so
//! the resolver never spins.

use std::collections::{HashMap, HashSet};

use super::{Crate, Module, ResolvedPath, Visibility};

/// Map from a re-export source path to its declared target path.
///
/// Built once per [`crate::Workspace`] load and queried by any consumer
/// that needs canonical names (architectural analyses, dependency
/// analyses, etc.).
///
/// # Scope: only `pub use` edges
///
/// `pub(crate) use`, `pub(super) use`, and bare `use` are deliberately
/// excluded. The rationale is that those forms tighten visibility instead
/// of re-publishing a name across the crate boundary; this index is built
/// to support cross-crate import reasoning.
///
/// **Consequence — known precision gap.** Chains that pass through a
/// `pub(crate) use` hop are not followed. Example:
///
/// ```ignore
/// // crate `data-models`
/// pub struct User;
///
/// // crate `data-api`
/// pub(crate) use data_models::User;    // hop NOT recorded
/// pub use Self::User as PublicUser;    // edge: data_api::PublicUser -> data_api::User
/// ```
///
/// A consumer asking for the canonical of `data_api::PublicUser` will get
/// `data_api::User` — not the true source `data_models::User`. Architecture
/// rules targeting `data_models::User` will not fire on imports of
/// `data_api::PublicUser`. Fixing this requires either also recording
/// intra-crate `pub(crate) use` edges (and filtering at query time by the
/// caller's crate) or doing a separate intra-crate resolution pass.
/// Neither is in scope for v1.
#[derive(Debug, Clone, Default)]
pub struct ReExportIndex {
    edges: HashMap<ResolvedPath, ResolvedPath>,
    /// All paths that appear as the *target* of some `pub use` edge — i.e.
    /// items reachable via re-export. Built once at construction so
    /// [`Self::is_target`] is O(1).
    targets: HashSet<ResolvedPath>,
}

impl ReExportIndex {
    /// Build the index from a set of workspace crates.
    pub fn build(crates: &[Crate]) -> Self {
        let mut edges = HashMap::new();
        for krate in crates {
            // Re-exports apply only to the crate's public API surface,
            // which lives in the lib (or proc-macro / main bin). Test and
            // build-script targets don't expose a stable API, so we skip
            // them here.
            if let Some(target) = krate.lib_or_main() {
                collect_edges(&target.root, &mut edges);
            }
        }
        let mut targets: HashSet<ResolvedPath> = edges.values().cloned().collect();
        // Expand `pub use M::*` glob re-exports: every public item of `M` is
        // re-exported into the crate's API, so mark each a target too — the
        // named-`pub use` exemption (see `is_target`), applied to globs. Gated
        // on the glob's own `pub` visibility (captured in `Module::glob_reexports`),
        // mirroring `collect_edges`. A name-by-name edge isn't recorded (the glob
        // doesn't rename), so `canonical()` is unaffected — only the exemption set
        // grows.
        let index = module_index(crates);
        for krate in crates {
            if let Some(target) = krate.lib_or_main() {
                collect_glob_targets(&target.root, &index, &mut targets);
            }
        }
        Self { edges, targets }
    }

    /// Follow the chain from `path` to its canonical definition.
    ///
    /// Returns `path` unchanged if there's no outgoing edge. Cycles are
    /// broken silently — the chain stops at the first repeated node and
    /// returns the path at that point.
    pub fn canonical(&self, path: &ResolvedPath) -> ResolvedPath {
        let mut current = path.clone();
        let mut visited: HashSet<ResolvedPath> = HashSet::new();
        visited.insert(current.clone());

        while let Some(next) = self.edges.get(&current) {
            if !visited.insert(next.clone()) {
                break;
            }
            current = next.clone();
        }
        current
    }

    /// Returns `true` if `path` is the target of any `pub use` edge in the
    /// index — i.e. some `pub use X;` in the workspace ultimately resolves
    /// to (or hops through) `path`.
    ///
    /// Structural-fix consumers gate on this before narrowing visibility:
    /// rewriting `pub X` to `pub(crate) X` would break the re-export
    /// (E0364 / E0365). Items that are *only* `pub use`'d (no other use)
    /// still appear as targets, so the lint should also skip narrowing for
    /// them or emit a coordinated multi-edit fix that narrows the
    /// `pub use` line in lockstep.
    pub fn is_target(&self, path: &ResolvedPath) -> bool {
        self.targets.contains(path)
    }

    /// Returns the number of `pub use` edges stored in the index.
    ///
    /// Mostly useful for tests; consumers query [`Self::canonical`]
    /// rather than introspecting the graph.
    pub fn len(&self) -> usize {
        self.edges.len()
    }

    /// Returns `true` if no `pub use` edges are stored.
    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }
}

fn collect_edges(module: &Module, edges: &mut HashMap<ResolvedPath, ResolvedPath>) {
    for binding in &module.use_bindings {
        if !matches!(binding.visibility, Visibility::Public) {
            continue;
        }
        let mut source_segs = module.canonical.segments().to_vec();
        source_segs.push(binding.local_name.clone());
        let source = ResolvedPath::new(source_segs);
        if source == binding.canonical {
            // `pub use self::X;` resolves source to itself; drop instead of
            // recording a degenerate cycle.
            continue;
        }
        edges.insert(source, binding.canonical.clone());
    }
    for sub in &module.submodules {
        collect_edges(sub, edges);
    }
}

/// Index every module (across all crate lib roots) by its canonical path, so a
/// glob re-export target (`pub use M::*` → `M`) can be looked up to enumerate
/// its public items.
fn module_index(crates: &[Crate]) -> HashMap<ResolvedPath, &Module> {
    let mut index = HashMap::new();
    for krate in crates {
        if let Some(target) = krate.lib_or_main() {
            index_module(&target.root, &mut index);
        }
    }
    index
}

fn index_module<'a>(module: &'a Module, index: &mut HashMap<ResolvedPath, &'a Module>) {
    index.insert(module.canonical.clone(), module);
    for sub in &module.submodules {
        index_module(sub, index);
    }
}

/// For each `pub use M::*` glob re-export in `module` (and its submodules), mark
/// every public item of `M` as a re-export target. `M`'s submodules aren't
/// recursed — a glob re-exports `M`'s direct public items (and submodule names),
/// not `M::sub::item`.
fn collect_glob_targets<'a>(
    module: &'a Module,
    index: &HashMap<ResolvedPath, &'a Module>,
    targets: &mut HashSet<ResolvedPath>,
) {
    for glob_target in &module.glob_reexports {
        if let Some(target_mod) = index.get(glob_target) {
            for item in &target_mod.items {
                if matches!(item.visibility, Visibility::Public) {
                    targets.insert(item.canonical.clone());
                }
            }
        }
    }
    for sub in &module.submodules {
        collect_glob_targets(sub, index, targets);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::use_tree::UseBinding;
    use crate::resolve::{Item, ItemKind, Visibility};

    fn module(
        canonical: &[&str],
        use_bindings: Vec<UseBinding>,
        submodules: Vec<Module>,
    ) -> Module {
        Module {
            name: canonical.last().copied().unwrap_or_default().to_string(),
            canonical: ResolvedPath::new(canonical.iter().map(|s| s.to_string())),
            visibility: Visibility::Public,
            items: Vec::new(),
            submodules,
            use_bindings,
            broken_mod_decls: Vec::new(),
            cfg_features: Vec::new(),
            occurrences: Vec::new(),
            glob_reexports: Vec::new(),
            signature_exposures: Vec::new(),
            file: None,
            doctest_crate_refs: std::collections::HashSet::new(),
        }
    }

    fn krate(name: &str, root: Module) -> Crate {
        let target = crate::resolve::Target {
            kind: crate::resolve::TargetKind::Lib,
            name: name.into(),
            src_path: std::path::PathBuf::from("src/lib.rs"),
            root,
        };
        Crate {
            name: name.into(),
            version: "0.0.0".into(),
            manifest_dir: std::path::PathBuf::new(),
            is_workspace_member: true,
            targets: vec![target],
            orphan_files: Vec::new(),
            declared_features: Vec::new(),
            feature_values: std::collections::BTreeMap::new(),
            manifest: crate::manifest::Manifest::empty(),
        }
    }

    fn pub_use(local: &str, canonical: &[&str]) -> UseBinding {
        UseBinding {
            local_name: local.into(),
            canonical: ResolvedPath::new(canonical.iter().map(|s| s.to_string())),
            visibility: Visibility::Public,
            source: None,
        }
    }

    fn private_use(local: &str, canonical: &[&str]) -> UseBinding {
        UseBinding {
            local_name: local.into(),
            canonical: ResolvedPath::new(canonical.iter().map(|s| s.to_string())),
            visibility: Visibility::Private,
            source: None,
        }
    }

    #[test]
    fn empty_workspace_has_no_edges() {
        let idx = ReExportIndex::build(&[]);
        assert!(idx.is_empty());
    }

    #[test]
    fn private_use_does_not_create_edge() {
        let m = module(
            &["demo"],
            vec![private_use("X", &["demo", "inner", "X"])],
            vec![],
        );
        let idx = ReExportIndex::build(&[krate("demo", m)]);
        assert!(idx.is_empty());
        // Unchanged: no edge to follow.
        let q = ResolvedPath::new(["demo", "X"]);
        assert_eq!(idx.canonical(&q), q);
    }

    fn pub_struct(canonical: &[&str], visibility: Visibility) -> Item {
        Item {
            name: canonical.last().copied().unwrap_or_default().to_string(),
            kind: ItemKind::Struct,
            visibility,
            canonical: ResolvedPath::new(canonical.iter().map(|s| s.to_string())),
            source: None,
            vis_byte_range: None,
        }
    }

    #[test]
    fn pub_glob_reexport_marks_target_items() {
        // `pub use crate::inner::*;` at the root re-exports `inner`'s *public*
        // items, so each must become a re-export target (regex's `Locations` FP
        // class — a public-API item reachable only via a glob). Private items
        // and items behind a plain (non-`pub`) glob are not re-exported.
        let mut inner = module(&["demo", "inner"], vec![], vec![]);
        inner.items = vec![
            pub_struct(&["demo", "inner", "Reexported"], Visibility::Public),
            pub_struct(&["demo", "inner", "Hidden"], Visibility::Private),
        ];
        let mut root = module(&["demo"], vec![], vec![inner]);
        root.glob_reexports = vec![ResolvedPath::new(["demo", "inner"])];

        let idx = ReExportIndex::build(&[krate("demo", root)]);
        assert!(
            idx.is_target(&ResolvedPath::new(["demo", "inner", "Reexported"])),
            "public item re-exported via glob should be a target"
        );
        assert!(
            !idx.is_target(&ResolvedPath::new(["demo", "inner", "Hidden"])),
            "private item must not be a glob re-export target"
        );
    }

    #[test]
    fn pub_use_creates_one_hop_chain() {
        // `pub use internal::User;` inside crate `data_models` root.
        let m = module(
            &["data_models"],
            vec![pub_use("User", &["data_models", "internal", "User"])],
            vec![],
        );
        let idx = ReExportIndex::build(&[krate("data_models", m)]);
        let q = ResolvedPath::new(["data_models", "User"]);
        let resolved = idx.canonical(&q);
        assert_eq!(resolved.display(), "data_models::internal::User");
    }

    #[test]
    fn pub_use_chases_chain_across_two_crates() {
        let crate_a_root = module(
            &["data_models"],
            vec![pub_use("User", &["data_models", "internal", "User"])],
            vec![],
        );
        let crate_b_root = module(
            &["data_api"],
            vec![pub_use("User", &["data_models", "User"])],
            vec![],
        );
        let idx = ReExportIndex::build(&[
            krate("data_models", crate_a_root),
            krate("data_api", crate_b_root),
        ]);

        let q = ResolvedPath::new(["data_api", "User"]);
        let resolved = idx.canonical(&q);
        assert_eq!(resolved.display(), "data_models::internal::User");
    }

    #[test]
    fn submodule_pub_use_is_walked() {
        let inner = module(
            &["demo", "inner"],
            vec![pub_use("X", &["demo", "deepest", "X"])],
            vec![],
        );
        let root = module(
            &["demo"],
            vec![pub_use("X", &["demo", "inner", "X"])],
            vec![inner],
        );
        let idx = ReExportIndex::build(&[krate("demo", root)]);
        let q = ResolvedPath::new(["demo", "X"]);
        assert_eq!(idx.canonical(&q).display(), "demo::deepest::X");
    }

    #[test]
    fn cycle_does_not_loop_forever() {
        // Pathological: A pub-uses B::X, B pub-uses A::X.
        let a = module(&["a"], vec![pub_use("X", &["b", "X"])], vec![]);
        let b = module(&["b"], vec![pub_use("X", &["a", "X"])], vec![]);
        let idx = ReExportIndex::build(&[krate("a", a), krate("b", b)]);

        let q = ResolvedPath::new(["a", "X"]);
        // Just needs to terminate; we don't care which endpoint it stops at
        // beyond that it's one of the cycle members.
        let resolved = idx.canonical(&q);
        let s = resolved.display();
        assert!(s == "a::X" || s == "b::X", "got {s}");
    }

    #[test]
    fn paths_with_no_edge_pass_through() {
        let m = module(
            &["demo"],
            vec![pub_use("X", &["demo", "internal", "X"])],
            vec![],
        );
        let idx = ReExportIndex::build(&[krate("demo", m)]);
        let q = ResolvedPath::new(["other", "Y"]);
        assert_eq!(idx.canonical(&q), q);
    }

    #[test]
    fn is_target_flags_pub_use_destinations() {
        // `pub use internal::User;` in crate `demo` makes
        // `demo::internal::User` a re-export target — narrowing it would
        // break the `pub use`.
        let m = module(
            &["demo"],
            vec![pub_use("User", &["demo", "internal", "User"])],
            vec![],
        );
        let idx = ReExportIndex::build(&[krate("demo", m)]);

        assert!(idx.is_target(&ResolvedPath::new(["demo", "internal", "User"])));
        // The source side (the re-exported name) is not itself a target.
        assert!(!idx.is_target(&ResolvedPath::new(["demo", "User"])));
        // Unrelated paths are not targets.
        assert!(!idx.is_target(&ResolvedPath::new(["other", "Thing"])));
    }

    #[test]
    fn is_target_covers_intermediate_chain_hops() {
        // Two-hop chain: data_api::User -> data_models::User -> data_models::internal::User.
        // Both the intermediate (data_models::User) and the leaf
        // (data_models::internal::User) are targets — narrowing either
        // breaks the chain.
        let crate_a_root = module(
            &["data_models"],
            vec![pub_use("User", &["data_models", "internal", "User"])],
            vec![],
        );
        let crate_b_root = module(
            &["data_api"],
            vec![pub_use("User", &["data_models", "User"])],
            vec![],
        );
        let idx = ReExportIndex::build(&[
            krate("data_models", crate_a_root),
            krate("data_api", crate_b_root),
        ]);

        assert!(idx.is_target(&ResolvedPath::new(["data_models", "User"])));
        assert!(idx.is_target(&ResolvedPath::new(["data_models", "internal", "User"])));
        assert!(!idx.is_target(&ResolvedPath::new(["data_api", "User"])));
    }

    // Sanity: ItemKind import is unused here, suppress.
    #[allow(dead_code)]
    fn _kinds_compile() -> ItemKind {
        ItemKind::Fn
    }
}
