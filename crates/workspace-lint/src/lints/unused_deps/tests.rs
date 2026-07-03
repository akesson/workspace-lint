//! Helper-level unit tests, split by backend: manifest-side helpers (dep
//! collection, delete suggestions) target the rustc backend's copies in
//! [`super::ir`] — the production path over the fast tier's `Manifest` — and
//! the set-based `find_unused_deps` semantics (separator fallback included)
//! target [`super::legacy`], which retains that shape until it retires. The
//! two backends' types are distinct on purpose (verbatim-copy strategy), so
//! each group speaks its own family.

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
    let s =
        super::ir::build_delete_suggestion(&m, &entry(DepSection::Dependencies, "rand")).unwrap();
    let start = s.span.byte_start as usize;
    let end = s.span.byte_end as usize;
    assert_eq!(&m.raw()[start..end], "rand = \"0.8\"\n");
}

#[test]
fn delete_consumes_crlf_after_dep_line() {
    let m = parse_manifest("[dependencies]\r\nrand = \"0.8\"\r\nfoo = \"1\"\r\n");
    let s =
        super::ir::build_delete_suggestion(&m, &entry(DepSection::Dependencies, "rand")).unwrap();
    let start = s.span.byte_start as usize;
    let end = s.span.byte_end as usize;
    assert_eq!(&m.raw()[start..end], "rand = \"0.8\"\r\n");
}
