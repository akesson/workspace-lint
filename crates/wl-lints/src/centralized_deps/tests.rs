use super::*;

/// A workspace-deps view where every named entry keeps cargo's default
/// `default-features = true`. [`ws_df`] declares deviating entries.
fn ws(names: &[&str]) -> std::collections::BTreeMap<String, bool> {
    names.iter().map(|s| (s.to_string(), true)).collect()
}

/// A workspace-deps view with explicit per-entry `default-features`.
fn ws_df(entries: &[(&str, bool)]) -> std::collections::BTreeMap<String, bool> {
    entries
        .iter()
        .map(|(name, df)| (name.to_string(), *df))
        .collect()
}

fn parse_item(toml_str: &str, section: DepSection, dep_name: &str) -> Item {
    let doc: wl_engine::fast::toml_edit::Document<String> =
        wl_engine::fast::toml_edit::Document::parse(toml_str.to_string()).unwrap();
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
    assert!(msg.unwrap().message.contains("use { workspace = true }"));
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
    assert!(
        msg.unwrap()
            .message
            .contains("not in [workspace.dependencies]")
    );
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
    assert!(msg.unwrap().message.contains("use { workspace = true }"));
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
    assert!(
        msg.unwrap()
            .message
            .contains("not in [workspace.dependencies]")
    );
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
    assert!(msg.unwrap().message.contains("own git source"));
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
    assert!(msg.message.contains("[dev-dependencies]"));
}

#[test]
fn build_suggestion_produces_machine_applicable_replacement() {
    let m = parse_manifest("[package]\nname = \"a\"\n\n[dependencies]\nserde = \"1.0\"\n");
    // key_in_workspace = true: the dep is already centralized, so the rewrite
    // is safe to auto-apply.
    let s = build_rewrite_suggestion(&m, DepSection::Dependencies, "serde", true).unwrap();
    assert_eq!(s.applicability, Applicability::MachineApplicable);
    assert_eq!(s.replacement, "serde = { workspace = true }");
    assert_eq!(
        &m.raw()[s.span.byte_start as usize..s.span.byte_end as usize],
        "serde = \"1.0\""
    );
}

#[test]
fn build_suggestion_not_in_workspace_is_maybe_incorrect() {
    // key_in_workspace = false: `serde = { workspace = true }` would reference a
    // nonexistent [workspace.dependencies] entry, so the suggestion is a preview
    // only — MaybeIncorrect so `--fix` skips it and never breaks the manifest.
    let m = parse_manifest("[package]\nname = \"a\"\n\n[dependencies]\nserde = \"1.0\"\n");
    let s = build_rewrite_suggestion(&m, DepSection::Dependencies, "serde", false).unwrap();
    assert_eq!(s.applicability, Applicability::MaybeIncorrect);
    assert_eq!(s.replacement, "serde = { workspace = true }");
}

#[test]
fn build_suggestion_preserves_features() {
    let m =
        parse_manifest("[dependencies]\nserde = { version = \"1\", features = [\"derive\"] }\n");
    let s = build_rewrite_suggestion(&m, DepSection::Dependencies, "serde", true).unwrap();
    assert_eq!(
        s.replacement,
        "serde = { workspace = true, features = [\"derive\"] }"
    );
}

#[test]
fn build_suggestion_returns_none_for_already_workspace() {
    let m = parse_manifest("[dependencies]\nserde = { workspace = true }\n");
    let s = build_rewrite_suggestion(&m, DepSection::Dependencies, "serde", true);
    assert!(s.is_none(), "expected no rewrite, got {s:?}");
}

/// Member `default-features = false` vs a workspace entry without it: the
/// rewrite is blocked (cargo would ignore the member flag — the feature set
/// silently changes) and the message says why.
#[test]
fn default_features_mismatch_blocks_rewrite() {
    let item = parse_item(
        "[dependencies]\ngix = { version = \"0.85\", default-features = false }\n",
        DepSection::Dependencies,
        "gix",
    );
    let issue = check_dep("gix", &item, DepSection::Dependencies, &ws(&["gix"])).unwrap();
    assert!(issue.rewrite_blocked);
    assert!(issue.insertable.is_none());
    assert!(
        issue.message.contains("`default-features` (false)"),
        "{}",
        issue.message
    );
    assert!(issue.message.contains("align the two declarations"));
}

/// The mirror direction: workspace entry says `default-features = false`,
/// member relies on the default (true). Inheriting would silently STRIP the
/// member's default features — equally blocked.
#[test]
fn default_features_mismatch_blocks_rewrite_inverse() {
    let item = parse_item(
        "[dependencies]\ngix = \"0.85\"\n",
        DepSection::Dependencies,
        "gix",
    );
    let issue = check_dep(
        "gix",
        &item,
        DepSection::Dependencies,
        &ws_df(&[("gix", false)]),
    )
    .unwrap();
    assert!(issue.rewrite_blocked);
}

/// Matching `default-features = false` on both sides: an ordinary
/// auto-fixable finding (the member rewrite preserves the key — explicit
/// agreement, no semantics change).
#[test]
fn default_features_agreement_is_not_blocked() {
    let item = parse_item(
        "[dependencies]\ngix = { version = \"0.85\", default-features = false }\n",
        DepSection::Dependencies,
        "gix",
    );
    let issue = check_dep(
        "gix",
        &item,
        DepSection::Dependencies,
        &ws_df(&[("gix", false)]),
    )
    .unwrap();
    assert!(!issue.rewrite_blocked);
    assert!(issue.message.contains("use { workspace = true }"));
}

/// A missing dep with `default-features = false` seeds the insertion WITH
/// the flag — the workspace entry is where cargo resolves it from
/// (member-side alone is ignored; the helix-gix breakage).
#[test]
fn missing_dep_carries_default_features_into_insertable() {
    let item = parse_item(
        "[dependencies]\ngix = { version = \"0.85\", default-features = false }\n",
        DepSection::Dependencies,
        "gix",
    );
    let issue = check_dep("gix", &item, DepSection::Dependencies, &ws(&[])).unwrap();
    assert_eq!(issue.insertable, Some(("0.85".to_string(), false)));
}
