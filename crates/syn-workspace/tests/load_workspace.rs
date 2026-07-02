//! Smoke test: `Workspace::load` against this very repository.
//!
//! Verifies the load pipeline runs end-to-end and discovers every member
//! crate. This test is intentionally fragile to workspace structure — adding
//! or removing a crate from `crates/*` requires updating the expected set,
//! which is the right tradeoff for catching accidental membership changes.

use std::path::PathBuf;

use syn_workspace::{Crate, Module, Target, Workspace};

#[test]
fn workspace_model_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Workspace>();
    assert_send_sync::<Crate>();
    assert_send_sync::<Target>();
    assert_send_sync::<Module>();
}

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is `<repo>/crates/syn-workspace`; jump two levels up.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

#[test]
fn load_discovers_all_workspace_members() {
    let ws = Workspace::load(workspace_root()).expect("load workspace");

    let mut names: Vec<_> = ws.crates().iter().map(|c| c.name.clone()).collect();
    names.sort();

    assert_eq!(
        names,
        vec![
            "syn-workspace",
            "syn-workspace-marker",
            "wl-ir",
            "workspace-lint",
            "workspace-lint-marker",
        ],
        "discovered crate names should match the current workspace members"
    );

    for krate in ws.crates() {
        assert!(
            krate.is_workspace_member,
            "{} should be flagged as workspace member",
            krate.name
        );
        assert!(
            krate.manifest_dir.join("Cargo.toml").exists(),
            "{} manifest dir must contain a Cargo.toml",
            krate.name
        );
        assert!(
            !krate.version.is_empty(),
            "{} should have a non-empty version",
            krate.name
        );
    }
}

#[test]
fn members_iterator_returns_only_workspace_members() {
    let ws = Workspace::load(workspace_root()).expect("load workspace");
    let members_count = ws.members().count();
    let crates_count = ws.crates().len();
    assert_eq!(
        members_count, crates_count,
        "v1 only materializes workspace members; external crates not yet loaded"
    );
}

#[test]
fn module_tree_is_populated_for_each_member() {
    let ws = Workspace::load(workspace_root()).expect("load workspace");

    // Every workspace member should expose a primary unit (lib or bin)
    // backed by a real source file.
    for krate in ws.crates() {
        let target = krate
            .lib_or_main()
            .unwrap_or_else(|| panic!("{}: should expose a lib_or_main target", krate.name));
        assert!(
            target.root.file.is_some(),
            "{}: primary target's root module should be backed by a file",
            krate.name
        );
    }

    let me = ws
        .member_by_name("syn-workspace")
        .expect("self crate should be a member");

    let lib = me.lib_or_main().expect("syn-workspace lib target");
    let sub_names: Vec<_> = lib
        .root
        .submodules
        .iter()
        .map(|m| m.name.as_str())
        .collect();
    for expected in ["resolve", "macros", "plugins"] {
        assert!(
            sub_names.contains(&expected),
            "syn-workspace root should have submodule {expected}, got {sub_names:?}"
        );
    }
}

#[test]
fn re_export_index_chases_self_chain() {
    let ws = Workspace::load(workspace_root()).expect("load workspace");

    // syn-workspace::lib.rs has `pub use resolve::ResolvedPath`, which Tier 2.5
    // should chase through `resolve`'s own `pub use types::ResolvedPath` to the
    // definition inside the `resolve::types` submodule. Canonical paths use the
    // code-form crate name (underscores), matching what `use syn_workspace::...`
    // writes in source.
    let exported_at_root = syn_workspace::ResolvedPath::new(["syn_workspace", "ResolvedPath"]);
    let canonical = ws.resolve_canonical(&exported_at_root);
    assert_eq!(
        canonical.display(),
        "syn_workspace::resolve::types::ResolvedPath",
        "pub use should chase to the definition site; got {}",
        canonical.display(),
    );
}

#[test]
fn module_tree_extracts_pub_items() {
    let ws = Workspace::load(workspace_root()).expect("load workspace");
    let me = ws.member_by_name("syn-workspace").expect("self crate");

    let lib = me.lib_or_main().expect("syn-workspace lib target");
    let resolve_mod = lib
        .root
        .submodules
        .iter()
        .find(|m| m.name == "resolve")
        .expect("resolve submodule");
    // The leaf value types live in `resolve::types`; `resolve/mod.rs` only
    // re-exports them. Assert the walker reaches the nested module and sees the
    // `pub struct` defined there.
    let types_mod = resolve_mod
        .submodules
        .iter()
        .find(|m| m.name == "types")
        .expect("resolve::types submodule");
    let pub_names: Vec<_> = types_mod
        .items
        .iter()
        .filter(|i| matches!(i.visibility, syn_workspace::Visibility::Public))
        .map(|i| i.name.as_str())
        .collect();
    assert!(
        pub_names.contains(&"ResolvedPath"),
        "resolve::types module should expose ResolvedPath; got {pub_names:?}"
    );
}
