//! Helper-level unit tests, split by backend: manifest-side helpers (dep
//! collection, delete suggestions) target the rustc backend's copies in
//! [`super::ir`] — the production path over the fast tier's `Manifest` — and
//! the set-based `find_unused_deps` semantics (separator fallback included)
//! target [`super::legacy`], which retains that shape until it retires. The
//! two backends' types are distinct on purpose (verbatim-copy strategy), so
//! each group speaks its own family.

use std::collections::{BTreeMap, HashSet};

use crate::diagnostic::Evidence;

// ── manifest-side helpers, over the fast tier's types (super::ir) ──────────

use wl_engine::fast::{DeclaredDep, DepSection, Manifest};

fn parse_manifest(content: &str) -> Manifest {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("Cargo.toml");
    std::fs::write(&p, content).unwrap();
    let _ = Box::leak(Box::new(dir));
    Manifest::load(&p).unwrap()
}

fn entry(section: DepSection, name: &str) -> DeclaredDep {
    DeclaredDep {
        section,
        original_name: name.into(),
        normalized_name: name.replace('-', "_"),
        target_gated: false,
    }
}

#[test]
fn collect_deps_basic() {
    let m = parse_manifest(
        r#"
[dependencies]
serde = "1"
tokio = { workspace = true }
"#,
    );
    let deps = super::ir::collect_deps(&m, &[]);
    assert!(deps.contains_key("serde"));
    assert!(deps.contains_key("tokio"));
}

#[test]
fn collect_deps_normalizes_hyphens() {
    let m = parse_manifest(
        r#"
[dependencies]
my-crate = "1"
"#,
    );
    let deps = super::ir::collect_deps(&m, &[]);
    assert!(deps.contains_key("my_crate"));
}

#[test]
fn collect_deps_respects_ignore() {
    let m = parse_manifest(
        r#"
[dependencies]
serde = "1"
prost = "0.12"
"#,
    );
    let deps = super::ir::collect_deps(&m, &["prost".into()]);
    assert!(deps.contains_key("serde"));
    assert!(!deps.contains_key("prost"));
}

#[test]
fn collect_deps_all_sections() {
    let m = parse_manifest(
        r#"
[dependencies]
a = "1"
[dev-dependencies]
b = "1"
[build-dependencies]
c = "1"
"#,
    );
    let deps = super::ir::collect_deps(&m, &[]);
    assert_eq!(deps.len(), 3);
    assert_eq!(deps["a"][0].section, DepSection::Dependencies);
    assert_eq!(deps["b"][0].section, DepSection::DevDependencies);
    assert_eq!(deps["c"][0].section, DepSection::BuildDependencies);
}

#[test]
fn delete_consumes_lf_after_dep_line() {
    let m = parse_manifest("[dependencies]\nrand = \"0.8\"\nfoo = \"1\"\n");
    let s = super::ir::build_delete_suggestion(&m, &entry(DepSection::Dependencies, "rand"), None)
        .unwrap();
    let start = s.span.byte_start as usize;
    let end = s.span.byte_end as usize;
    assert_eq!(&m.raw()[start..end], "rand = \"0.8\"\n");
}

#[test]
fn delete_consumes_crlf_after_dep_line() {
    let m = parse_manifest("[dependencies]\r\nrand = \"0.8\"\r\nfoo = \"1\"\r\n");
    let s = super::ir::build_delete_suggestion(&m, &entry(DepSection::Dependencies, "rand"), None)
        .unwrap();
    let start = s.span.byte_start as usize;
    let end = s.span.byte_end as usize;
    assert_eq!(&m.raw()[start..end], "rand = \"0.8\"\r\n");
}

#[test]
fn delete_suggestion_carries_supplied_evidence() {
    // The evidence payload threads through onto the suggestion so downstream
    // verification can match this dep's package identity.
    let m = parse_manifest("[dependencies]\nrand = \"0.8\"\n");
    let evidence = Evidence::DepUnused {
        krate_code: "demo".into(),
        package_name: "rand".into(),
    };
    let s = super::ir::build_delete_suggestion(
        &m,
        &entry(DepSection::Dependencies, "rand"),
        Some(evidence.clone()),
    )
    .unwrap();
    assert_eq!(s.evidence, Some(evidence));
}

// ── set-based find_unused_deps semantics (super::legacy) ───────────────────

use syn_workspace::manifest::{DeclaredDep as SynDep, DepSection as SynSection};

fn syn_entry(section: SynSection, name: &str) -> SynDep {
    SynDep {
        section,
        original_name: name.into(),
        normalized_name: name.replace('-', "_"),
    }
}

#[test]
fn find_unused_all_used() {
    let mut deps = BTreeMap::new();
    deps.insert(
        "serde".into(),
        vec![syn_entry(SynSection::Dependencies, "serde")],
    );
    let mut refs = HashSet::new();
    refs.insert("serde".into());
    assert!(super::legacy::find_unused_deps(deps, &refs).is_empty());
}

#[test]
fn find_unused_none_used() {
    let mut deps = BTreeMap::new();
    deps.insert(
        "serde".into(),
        vec![syn_entry(SynSection::Dependencies, "serde")],
    );
    let refs = HashSet::new();
    let unused = super::legacy::find_unused_deps(deps, &refs);
    assert_eq!(unused, vec![syn_entry(SynSection::Dependencies, "serde")]);
}

#[test]
fn find_unused_partial() {
    let mut deps = BTreeMap::new();
    deps.insert(
        "serde".into(),
        vec![syn_entry(SynSection::Dependencies, "serde")],
    );
    deps.insert(
        "rand".into(),
        vec![syn_entry(SynSection::Dependencies, "rand")],
    );
    let mut refs = HashSet::new();
    refs.insert("serde".into());
    let unused = super::legacy::find_unused_deps(deps, &refs);
    assert_eq!(unused, vec![syn_entry(SynSection::Dependencies, "rand")]);
}

#[test]
fn find_unused_md5_libname_suppressed_by_separator_fallback() {
    // Package `md-5` normalizes to `md_5`, but its lib target is `md5`, so the
    // only reference is to `md5`. The H3 separator-insensitive fallback matches.
    // (The rustc backend covers this case structurally: the resolve graph maps
    // package → lib-target name — see wl-engine's dep-usage tests.)
    let mut deps = BTreeMap::new();
    deps.insert(
        "md_5".into(),
        vec![syn_entry(SynSection::Dependencies, "md-5")],
    );
    let mut refs = HashSet::new();
    refs.insert("md5".into());
    assert!(super::legacy::find_unused_deps(deps, &refs).is_empty());
}

#[test]
fn find_unused_genuinely_unused_dep_still_flagged() {
    // No reference under any separator form → still reported.
    let mut deps = BTreeMap::new();
    deps.insert(
        "md_5".into(),
        vec![syn_entry(SynSection::Dependencies, "md-5")],
    );
    let refs = HashSet::new();
    let unused = super::legacy::find_unused_deps(deps, &refs);
    assert_eq!(unused, vec![syn_entry(SynSection::Dependencies, "md-5")]);
}

#[test]
fn find_unused_separator_fallback_overmatches_safely() {
    // `my_crate` and `mycrate` collapse to the same stripped form, so a ref to
    // `mycrate` suppresses the `my_crate` dep. This is the documented, FP-safe
    // over-match: it can only hide an unused dep, never invent one.
    let mut deps = BTreeMap::new();
    deps.insert(
        "my_crate".into(),
        vec![syn_entry(SynSection::Dependencies, "my-crate")],
    );
    let mut refs = HashSet::new();
    refs.insert("mycrate".into());
    assert!(super::legacy::find_unused_deps(deps, &refs).is_empty());
}
