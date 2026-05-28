use super::*;

fn ws(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|s| s.to_string()).collect()
}

fn parse_item(toml_str: &str, section: DepSection, dep_name: &str) -> Item {
    let doc: syn_workspace::toml_edit::ImDocument<String> =
        syn_workspace::toml_edit::ImDocument::parse(toml_str.to_string()).unwrap();
    let table = doc.as_table();
    let section_item = match section {
        DepSection::Dependencies => table.get("dependencies"),
        DepSection::DevDependencies => table.get("dev-dependencies"),
        DepSection::BuildDependencies => table.get("build-dependencies"),
        DepSection::WorkspaceDependencies => table
            .get("workspace")
            .and_then(Item::as_table_like)
            .and_then(|t| t.get("dependencies")),
    }
    .unwrap();
    section_item
        .as_table_like()
        .unwrap()
        .get(dep_name)
        .unwrap()
        .clone()
}

fn parse_manifest(content: &str) -> Manifest {
    let path = std::path::PathBuf::from("/tmp/Cargo.toml");
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("Cargo.toml");
    let mut f = std::fs::File::create(&p).unwrap();
    f.write_all(content.as_bytes()).unwrap();
    drop(f);
    let m = Manifest::load(&p).unwrap();
    assert_eq!(m.raw(), content);
    let _ = path;
    m
}

#[test]
fn string_version_in_workspace() {
    let item = parse_item(
        "[dependencies]\nserde = \"1.0\"\n",
        DepSection::Dependencies,
        "serde",
    );
    let msg = check_dep("serde", &item, DepSection::Dependencies, &ws(&["serde"]));
    assert!(msg.is_some());
    assert!(msg.unwrap().contains("use { workspace = true }"));
}

#[test]
fn string_version_not_in_workspace() {
    let item = parse_item(
        "[dependencies]\nrand = \"1.0\"\n",
        DepSection::Dependencies,
        "rand",
    );
    let msg = check_dep("rand", &item, DepSection::Dependencies, &ws(&["serde"]));
    assert!(msg.is_some());
    assert!(msg.unwrap().contains("not in [workspace.dependencies]"));
}

#[test]
fn workspace_true_is_ok() {
    let item = parse_item(
        "[dependencies]\nserde = { workspace = true }\n",
        DepSection::Dependencies,
        "serde",
    );
    assert!(check_dep("serde", &item, DepSection::Dependencies, &ws(&["serde"])).is_none());
}

#[test]
fn path_dep_is_ok() {
    let item = parse_item(
        "[dependencies]\nother = { path = \"../other\" }\n",
        DepSection::Dependencies,
        "other",
    );
    assert!(check_dep("other", &item, DepSection::Dependencies, &ws(&["serde"])).is_none());
}

#[test]
fn table_version_in_workspace() {
    let item = parse_item(
        "[dependencies]\nserde = { version = \"1\" }\n",
        DepSection::Dependencies,
        "serde",
    );
    let msg = check_dep("serde", &item, DepSection::Dependencies, &ws(&["serde"]));
    assert!(msg.is_some());
    assert!(msg.unwrap().contains("use { workspace = true }"));
}

#[test]
fn table_version_not_in_workspace() {
    let item = parse_item(
        "[dependencies]\nserde = { version = \"1\" }\n",
        DepSection::Dependencies,
        "serde",
    );
    let msg = check_dep("serde", &item, DepSection::Dependencies, &ws(&[]));
    assert!(msg.is_some());
    assert!(msg.unwrap().contains("not in [workspace.dependencies]"));
}

#[test]
fn git_dep_in_workspace() {
    let item = parse_item(
        "[dependencies]\nbar = { git = \"https://github.com/foo/bar\" }\n",
        DepSection::Dependencies,
        "bar",
    );
    let msg = check_dep("bar", &item, DepSection::Dependencies, &ws(&["bar"]));
    assert!(msg.is_some());
    assert!(msg.unwrap().contains("own git source"));
}

#[test]
fn git_dep_not_in_workspace() {
    let item = parse_item(
        "[dependencies]\nbar = { git = \"https://github.com/foo/bar\" }\n",
        DepSection::Dependencies,
        "bar",
    );
    assert!(check_dep("bar", &item, DepSection::Dependencies, &ws(&[])).is_none());
}

#[test]
fn section_appears_in_message() {
    let item = parse_item(
        "[dev-dependencies]\nfoo = \"1.0\"\n",
        DepSection::DevDependencies,
        "foo",
    );
    let msg = check_dep("foo", &item, DepSection::DevDependencies, &ws(&[])).unwrap();
    assert!(msg.contains("[dev-dependencies]"));
}

#[test]
fn build_suggestion_produces_machine_applicable_replacement() {
    let m = parse_manifest("[package]\nname = \"a\"\n\n[dependencies]\nserde = \"1.0\"\n");
    let s = build_rewrite_suggestion(&m, DepSection::Dependencies, "serde").unwrap();
    assert_eq!(s.applicability, Applicability::MachineApplicable);
    assert_eq!(s.replacement, "serde = { workspace = true }");
    assert_eq!(
        &m.raw()[s.span.byte_start as usize..s.span.byte_end as usize],
        "serde = \"1.0\""
    );
}

#[test]
fn build_suggestion_preserves_features() {
    let m =
        parse_manifest("[dependencies]\nserde = { version = \"1\", features = [\"derive\"] }\n");
    let s = build_rewrite_suggestion(&m, DepSection::Dependencies, "serde").unwrap();
    assert_eq!(
        s.replacement,
        "serde = { workspace = true, features = [\"derive\"] }"
    );
}

#[test]
fn build_suggestion_returns_none_for_already_workspace() {
    let m = parse_manifest("[dependencies]\nserde = { workspace = true }\n");
    let s = build_rewrite_suggestion(&m, DepSection::Dependencies, "serde");
    assert!(s.is_none(), "expected no rewrite, got {s:?}");
}
