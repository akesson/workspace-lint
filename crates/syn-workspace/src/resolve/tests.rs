//! Resolved-model unit tests, split out of `mod.rs`.

use std::path::PathBuf;

use super::*;

fn item(name: &str, krate: &str) -> Item {
    Item {
        name: name.into(),
        kind: ItemKind::Fn,
        visibility: Visibility::Public,
        canonical: ResolvedPath::new([krate.to_string(), name.to_string()]),
        source: None,
        vis_byte_range: None,
    }
}

fn module(name: &str, krate: &str, items: Vec<Item>, submodules: Vec<Module>) -> Module {
    Module {
        name: name.into(),
        canonical: ResolvedPath::new([krate.to_string(), name.to_string()]),
        visibility: Visibility::Public,
        items,
        submodules,
        use_bindings: Vec::new(),
        broken_mod_decls: Vec::new(),
        cfg_features: Vec::new(),
        occurrences: Vec::new(),
        glob_reexports: Vec::new(),
        file: None,
        doctest_crate_refs: std::collections::HashSet::new(),
    }
}

#[test]
fn resolved_path_display_joins_segments() {
    let p = ResolvedPath::new(["serde", "de", "Deserialize"]);
    assert_eq!(p.display(), "serde::de::Deserialize");
    assert_eq!(p.crate_name(), Some("serde"));
}

#[test]
fn module_items_walks_tree_in_order() {
    let leaf = module("leaf", "demo", vec![item("inner", "demo")], vec![]);
    let root = module(
        "root",
        "demo",
        vec![item("a", "demo"), item("b", "demo")],
        vec![leaf],
    );
    let names: Vec<_> = root.walk_items().map(|(_, i)| i.name.clone()).collect();
    assert_eq!(names, vec!["a", "b", "inner"]);
}

#[test]
fn crate_pub_items_filters_visibility() {
    let mut pub_item = item("visible", "demo");
    let mut priv_item = item("hidden", "demo");
    priv_item.visibility = Visibility::Private;
    pub_item.visibility = Visibility::Public;
    let root = module("root", "demo", vec![pub_item, priv_item], vec![]);
    let lib_target = Target {
        kind: TargetKind::Lib,
        name: "demo".into(),
        src_path: PathBuf::from("src/lib.rs"),
        root,
    };
    let krate = Crate {
        name: "demo".into(),
        version: "0.0.0".into(),
        manifest_dir: PathBuf::new(),
        is_workspace_member: true,
        targets: vec![lib_target],
        orphan_files: Vec::new(),
        declared_features: Vec::new(),
        feature_values: std::collections::BTreeMap::new(),
        manifest: crate::manifest::Manifest::empty(),
    };
    let pub_names: Vec<_> = krate.pub_items().map(|i| i.name.clone()).collect();
    assert_eq!(pub_names, vec!["visible"]);
}
