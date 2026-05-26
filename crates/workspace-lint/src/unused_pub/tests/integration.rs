use super::helpers::*;
use crate::config::UnusedPubConfig;
use crate::unused_pub::check;
use protobuf::Message;
use scip::types::symbol_information::Kind;
use scip::types::{Index, SymbolRole};

fn write_scip_index(index: &Index) -> tempfile::NamedTempFile {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let bytes = index.write_to_bytes().unwrap();
    std::fs::write(tmp.path(), bytes).unwrap();
    tmp
}

fn make_config_with_index(
    scip_path: &str,
    exclude_crates: Vec<String>,
    allowlist: Vec<String>,
    kinds: Vec<String>,
) -> UnusedPubConfig {
    UnusedPubConfig {
        on_ci_only: false,
        scip_index: Some(scip_path.to_string()),
        exclude_crates,
        allowlist,
        kinds,
        exclude_paths: vec![],
        cargo_features: Default::default(),
    }
}

#[test]
fn integration_unused_pub_fn_reported() {
    let dir = tempfile::TempDir::new().unwrap();
    let src_path = dir.path().join("src/lib.rs");
    std::fs::create_dir_all(src_path.parent().unwrap()).unwrap();
    std::fs::write(&src_path, "pub fn unused() {}\n").unwrap();

    let full_path = src_path.to_str().unwrap();
    let sym = "rust-analyzer cargo mycrate 0.1.0 mycrate/unused()";

    let doc = make_doc(
        full_path,
        vec![make_symbol(sym, Kind::Function, "unused")],
        vec![make_occurrence(sym, SymbolRole::Definition as i32, 0)],
    );
    let index = make_index(vec![doc]);
    let tmp = write_scip_index(&index);

    let config = make_config_with_index(tmp.path().to_str().unwrap(), vec![], vec![], vec![]);
    let issues = check(&config);
    assert_eq!(issues.len(), 1);
    assert!(issues[0].title.contains("mycrate"));
}

#[test]
fn integration_same_crate_reference_suggests_pub_crate() {
    let dir = tempfile::TempDir::new().unwrap();
    let src_path = dir.path().join("src/lib.rs");
    std::fs::create_dir_all(src_path.parent().unwrap()).unwrap();
    std::fs::write(&src_path, "pub fn used() {}\n").unwrap();

    let full_path = src_path.to_str().unwrap();
    let sym = "rust-analyzer cargo mycrate 0.1.0 mycrate/used()";

    let doc = make_doc(
        full_path,
        vec![make_symbol(sym, Kind::Function, "used")],
        vec![
            make_occurrence(sym, SymbolRole::Definition as i32, 0),
            make_occurrence(sym, 0, 5), // same-crate reference
        ],
    );
    let index = make_index(vec![doc]);
    let tmp = write_scip_index(&index);

    let config = make_config_with_index(tmp.path().to_str().unwrap(), vec![], vec![], vec![]);
    let issues = check(&config);
    assert_eq!(issues.len(), 1);
    assert!(issues[0].title.contains("pub(crate)"));
    assert!(issues[0].details.iter().any(|d| d.contains("`used`")));
}

#[test]
fn integration_cross_crate_reference_not_reported() {
    let dir = tempfile::TempDir::new().unwrap();
    let src_path = dir.path().join("src/lib.rs");
    std::fs::create_dir_all(src_path.parent().unwrap()).unwrap();
    std::fs::write(&src_path, "pub fn used() {}\n").unwrap();

    let full_path = src_path.to_str().unwrap();
    let sym = "rust-analyzer cargo mycrate 0.1.0 mycrate/used()";

    // Definition in crate "mycrate"
    let doc_a = make_doc(
        full_path,
        vec![make_symbol(sym, Kind::Function, "used")],
        vec![make_occurrence(sym, SymbolRole::Definition as i32, 0)],
    );
    // Reference from a different crate
    let other_sym = "rust-analyzer cargo othercrate 0.1.0 othercrate/bar()";
    let doc_b = make_doc(
        "other/src/lib.rs",
        vec![make_symbol(other_sym, Kind::Function, "bar")],
        vec![
            make_occurrence(other_sym, SymbolRole::Definition as i32, 0),
            make_occurrence(sym, 0, 3), // cross-crate reference to mycrate/used
        ],
    );
    let index = make_index(vec![doc_a, doc_b]);
    let tmp = write_scip_index(&index);

    let config = make_config_with_index(tmp.path().to_str().unwrap(), vec![], vec![], vec![]);
    let issues = check(&config);
    assert!(issues.is_empty());
}

#[test]
fn integration_private_unused_not_reported() {
    let dir = tempfile::TempDir::new().unwrap();
    let src_path = dir.path().join("src/lib.rs");
    std::fs::create_dir_all(src_path.parent().unwrap()).unwrap();
    std::fs::write(&src_path, "fn private() {}\n").unwrap();

    let full_path = src_path.to_str().unwrap();
    let sym = "rust-analyzer cargo mycrate 0.1.0 mycrate/private()";

    let doc = make_doc(
        full_path,
        vec![make_symbol(sym, Kind::Function, "private")],
        vec![make_occurrence(sym, SymbolRole::Definition as i32, 0)],
    );
    let index = make_index(vec![doc]);
    let tmp = write_scip_index(&index);

    let config = make_config_with_index(tmp.path().to_str().unwrap(), vec![], vec![], vec![]);
    let issues = check(&config);
    assert!(issues.is_empty());
}

#[test]
fn integration_pub_crate_not_reported() {
    let dir = tempfile::TempDir::new().unwrap();
    let src_path = dir.path().join("src/lib.rs");
    std::fs::create_dir_all(src_path.parent().unwrap()).unwrap();
    std::fs::write(&src_path, "pub(crate) fn scoped() {}\n").unwrap();

    let full_path = src_path.to_str().unwrap();
    let sym = "rust-analyzer cargo mycrate 0.1.0 mycrate/scoped()";

    let doc = make_doc(
        full_path,
        vec![make_symbol(sym, Kind::Function, "scoped")],
        vec![make_occurrence(sym, SymbolRole::Definition as i32, 0)],
    );
    let index = make_index(vec![doc]);
    let tmp = write_scip_index(&index);

    let config = make_config_with_index(tmp.path().to_str().unwrap(), vec![], vec![], vec![]);
    let issues = check(&config);
    assert!(issues.is_empty());
}

#[test]
fn integration_trait_impl_excluded() {
    let dir = tempfile::TempDir::new().unwrap();
    let src_path = dir.path().join("src/lib.rs");
    std::fs::create_dir_all(src_path.parent().unwrap()).unwrap();
    std::fs::write(
        &src_path,
        "pub trait T { fn f(); }\npub struct S;\nimpl T for S {\n    pub fn f() {}\n}\n",
    )
    .unwrap();

    let full_path = src_path.to_str().unwrap();
    let sym = "rust-analyzer cargo mycrate 0.1.0 mycrate/S#T#f()";

    let doc = make_doc(
        full_path,
        vec![make_symbol_with_rels(
            sym,
            Kind::Method,
            "f",
            vec![make_relationship(true)],
        )],
        vec![make_occurrence(sym, SymbolRole::Definition as i32, 3)],
    );
    let index = make_index(vec![doc]);
    let tmp = write_scip_index(&index);

    let config = make_config_with_index(tmp.path().to_str().unwrap(), vec![], vec![], vec![]);
    let issues = check(&config);
    assert!(issues.is_empty());
}

#[test]
fn integration_test_fn_excluded() {
    let dir = tempfile::TempDir::new().unwrap();
    let src_path = dir.path().join("src/lib.rs");
    std::fs::create_dir_all(src_path.parent().unwrap()).unwrap();
    std::fs::write(&src_path, "pub fn test_something() {}\n").unwrap();

    let full_path = src_path.to_str().unwrap();
    let sym = "rust-analyzer cargo mycrate 0.1.0 mycrate/test_something()";
    let roles = SymbolRole::Definition as i32 | SymbolRole::Test as i32;

    let doc = make_doc(
        full_path,
        vec![make_symbol(sym, Kind::Function, "test_something")],
        vec![make_occurrence(sym, roles, 0)],
    );
    let index = make_index(vec![doc]);
    let tmp = write_scip_index(&index);

    let config = make_config_with_index(tmp.path().to_str().unwrap(), vec![], vec![], vec![]);
    let issues = check(&config);
    assert!(issues.is_empty());
}

#[test]
fn integration_exclude_crates_works() {
    let dir = tempfile::TempDir::new().unwrap();
    let src_path = dir.path().join("src/lib.rs");
    std::fs::create_dir_all(src_path.parent().unwrap()).unwrap();
    std::fs::write(&src_path, "pub fn unused() {}\n").unwrap();

    let full_path = src_path.to_str().unwrap();
    let sym = "rust-analyzer cargo api 0.1.0 api/unused()";

    let doc = make_doc(
        full_path,
        vec![make_symbol(sym, Kind::Function, "unused")],
        vec![make_occurrence(sym, SymbolRole::Definition as i32, 0)],
    );
    let index = make_index(vec![doc]);
    let tmp = write_scip_index(&index);

    let config = make_config_with_index(
        tmp.path().to_str().unwrap(),
        vec!["api".to_string()],
        vec![],
        vec![],
    );
    let issues = check(&config);
    assert!(issues.is_empty());
}

#[test]
fn integration_allowlist_works() {
    let dir = tempfile::TempDir::new().unwrap();
    let src_path = dir.path().join("src/lib.rs");
    std::fs::create_dir_all(src_path.parent().unwrap()).unwrap();
    std::fs::write(&src_path, "pub fn Error() {}\n").unwrap();

    let full_path = src_path.to_str().unwrap();
    let sym = "rust-analyzer cargo mycrate 0.1.0 mycrate/Error()";

    let doc = make_doc(
        full_path,
        vec![make_symbol(sym, Kind::Function, "Error")],
        vec![make_occurrence(sym, SymbolRole::Definition as i32, 0)],
    );
    let index = make_index(vec![doc]);
    let tmp = write_scip_index(&index);

    let config = make_config_with_index(
        tmp.path().to_str().unwrap(),
        vec![],
        vec!["Error".to_string()],
        vec![],
    );
    let issues = check(&config);
    assert!(issues.is_empty());
}

#[test]
fn integration_kinds_filter() {
    let dir = tempfile::TempDir::new().unwrap();
    let src_path = dir.path().join("src/lib.rs");
    std::fs::create_dir_all(src_path.parent().unwrap()).unwrap();
    std::fs::write(
        &src_path,
        "pub fn unused_fn() {}\npub struct UnusedStruct;\n",
    )
    .unwrap();

    let full_path = src_path.to_str().unwrap();
    let sym_fn = "rust-analyzer cargo mycrate 0.1.0 mycrate/unused_fn()";
    let sym_struct = "rust-analyzer cargo mycrate 0.1.0 mycrate/UnusedStruct#";

    let doc = make_doc(
        full_path,
        vec![
            make_symbol(sym_fn, Kind::Function, "unused_fn"),
            make_symbol(sym_struct, Kind::Struct, "UnusedStruct"),
        ],
        vec![
            make_occurrence(sym_fn, SymbolRole::Definition as i32, 0),
            make_occurrence(sym_struct, SymbolRole::Definition as i32, 1),
        ],
    );
    let index = make_index(vec![doc]);
    let tmp = write_scip_index(&index);

    let config = make_config_with_index(
        tmp.path().to_str().unwrap(),
        vec![],
        vec![],
        vec!["struct".to_string()],
    );
    let issues = check(&config);
    assert_eq!(issues.len(), 1);
    // Should only report the struct, not the function
    assert!(issues[0].details.iter().any(|d| d.contains("UnusedStruct")));
    assert!(!issues[0].details.iter().any(|d| d.contains("unused_fn")));
}

#[test]
fn integration_issues_grouped_by_crate() {
    let dir = tempfile::TempDir::new().unwrap();
    let src_a = dir.path().join("src/a.rs");
    let src_b = dir.path().join("src/b.rs");
    std::fs::create_dir_all(src_a.parent().unwrap()).unwrap();
    std::fs::write(&src_a, "pub fn a_unused() {}\n").unwrap();
    std::fs::write(&src_b, "pub fn b_unused() {}\n").unwrap();

    let path_a = src_a.to_str().unwrap();
    let path_b = src_b.to_str().unwrap();
    let sym_a = "rust-analyzer cargo crate-a 0.1.0 crate_a/a_unused()";
    let sym_b = "rust-analyzer cargo crate-b 0.1.0 crate_b/b_unused()";

    let doc_a = make_doc(
        path_a,
        vec![make_symbol(sym_a, Kind::Function, "a_unused")],
        vec![make_occurrence(sym_a, SymbolRole::Definition as i32, 0)],
    );
    let doc_b = make_doc(
        path_b,
        vec![make_symbol(sym_b, Kind::Function, "b_unused")],
        vec![make_occurrence(sym_b, SymbolRole::Definition as i32, 0)],
    );
    let index = make_index(vec![doc_a, doc_b]);
    let tmp = write_scip_index(&index);

    let config = make_config_with_index(tmp.path().to_str().unwrap(), vec![], vec![], vec![]);
    let issues = check(&config);
    assert_eq!(issues.len(), 2);

    let titles: Vec<&str> = issues.iter().map(|i| i.title.as_str()).collect();
    assert!(titles.iter().any(|t| t.contains("crate-a")));
    assert!(titles.iter().any(|t| t.contains("crate-b")));
}

#[test]
fn integration_removal_section_listed_before_tighten_section() {
    let dir = tempfile::TempDir::new().unwrap();
    let src_path = dir.path().join("src/lib.rs");
    std::fs::create_dir_all(src_path.parent().unwrap()).unwrap();
    std::fs::write(&src_path, "pub fn unused() {}\npub fn used() {}\n").unwrap();

    let full_path = src_path.to_str().unwrap();
    let sym_unused = "rust-analyzer cargo mycrate 0.1.0 mycrate/unused()";
    let sym_used = "rust-analyzer cargo mycrate 0.1.0 mycrate/used()";

    let doc = make_doc(
        full_path,
        vec![
            make_symbol(sym_unused, Kind::Function, "unused"),
            make_symbol(sym_used, Kind::Function, "used"),
        ],
        vec![
            make_occurrence(sym_unused, SymbolRole::Definition as i32, 0),
            make_occurrence(sym_used, SymbolRole::Definition as i32, 1),
            make_occurrence(sym_used, 0, 5), // same-crate reference
        ],
    );
    let index = make_index(vec![doc]);
    let tmp = write_scip_index(&index);

    let config = make_config_with_index(tmp.path().to_str().unwrap(), vec![], vec![], vec![]);
    let issues = check(&config);
    assert_eq!(issues.len(), 2);
    assert!(issues[0].title.contains("removal candidate"));
    assert!(issues[1].title.contains("pub(crate)"));
}

#[test]
fn integration_exclude_paths_works() {
    let dir = tempfile::TempDir::new().unwrap();
    let src_path = dir.path().join("generated/src/lib.rs");
    std::fs::create_dir_all(src_path.parent().unwrap()).unwrap();
    std::fs::write(&src_path, "pub fn unused() {}\n").unwrap();

    let full_path = src_path.to_str().unwrap();
    let sym = "rust-analyzer cargo mycrate 0.1.0 mycrate/unused()";

    let doc = make_doc(
        full_path,
        vec![make_symbol(sym, Kind::Function, "unused")],
        vec![make_occurrence(sym, SymbolRole::Definition as i32, 0)],
    );
    let index = make_index(vec![doc]);
    let tmp = write_scip_index(&index);

    let config = UnusedPubConfig {
        on_ci_only: false,
        scip_index: Some(tmp.path().to_str().unwrap().to_string()),
        exclude_crates: vec![],
        allowlist: vec![],
        kinds: vec![],
        exclude_paths: vec!["**/generated/**".to_string()],
        cargo_features: Default::default(),
    };
    let issues = check(&config);
    assert!(issues.is_empty());
}

#[test]
fn integration_empty_index_no_issues() {
    let index = make_index(vec![]);
    let tmp = write_scip_index(&index);

    let config = make_config_with_index(tmp.path().to_str().unwrap(), vec![], vec![], vec![]);
    let issues = check(&config);
    assert!(issues.is_empty());
}

#[test]
fn on_ci_only_skips_when_ci_not_set() {
    // Ensure CI is not set for this test
    // SAFETY: This test is single-threaded and no other code reads CI concurrently
    unsafe { std::env::remove_var("CI") };
    let config = UnusedPubConfig {
        on_ci_only: true,
        // No valid scip_index needed — should return early before reading it
        scip_index: Some("/nonexistent/index.scip".to_string()),
        exclude_crates: vec![],
        allowlist: vec![],
        kinds: vec![],
        exclude_paths: vec![],
        cargo_features: Default::default(),
    };
    let issues = check(&config);
    assert!(issues.is_empty());
}
