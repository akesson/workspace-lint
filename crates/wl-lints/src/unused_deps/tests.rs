//! Helper-level unit tests for [`super::ir`]'s manifest-side helpers (dep
//! collection, delete suggestions) over the fast tier's `Manifest`.

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

// ── the verdict join (partition_by_verdict) ────────────────────────────────

use std::collections::HashSet;
use wl_engine::semantic::{CrateDeps, DepKind, NotJudged, NotJudgedDep, UnusedDep};

fn verdict(unused: &[(&str, DepKind)], not_judged: &[(&str, DepKind, NotJudged)]) -> CrateDeps {
    CrateDeps {
        krate: "alpha".into(),
        unused: unused
            .iter()
            .map(|(n, k)| UnusedDep {
                name: n.to_string(),
                kind: *k,
            })
            .collect(),
        not_judged: not_judged
            .iter()
            .map(|(n, k, r)| NotJudgedDep {
                name: n.to_string(),
                kind: *k,
                reason: *r,
            })
            .collect(),
    }
}

#[test]
fn partition_routes_unused_and_not_compiled_by_name_and_kind() {
    let m = parse_manifest(
        "[dependencies]\nrand = \"0.8\"\n[dev-dependencies]\nrand = \"0.8\"\n[build-dependencies]\ncc = \"1\"\n",
    );
    let deps = super::ir::collect_deps(&m, &[]);
    // The NORMAL `rand` is unused; the DEV `rand` was exercised — the kinded
    // join must not conflate the two entries of the same package.
    let v = verdict(&[("rand", DepKind::Normal)], &[]);
    let (unused, nc) = super::ir::partition_by_verdict(deps, &v, &HashSet::new(), &m, &m);
    assert_eq!(unused.len(), 1);
    assert_eq!(unused[0].section, DepSection::Dependencies);
    assert!(nc.is_empty());

    // NotCompiled routes to the coverage bucket, not the findings.
    let deps = super::ir::collect_deps(&m, &[]);
    let v = verdict(&[], &[("rand", DepKind::Normal, NotJudged::NotCompiled)]);
    let (unused, nc) = super::ir::partition_by_verdict(deps, &v, &HashSet::new(), &m, &m);
    assert!(unused.is_empty());
    assert_eq!(nc.len(), 1);

    // Other exemption reasons surface nowhere.
    let deps = super::ir::collect_deps(&m, &[]);
    let v = verdict(&[], &[("cc", DepKind::Build, NotJudged::BuildDep)]);
    let (unused, nc) = super::ir::partition_by_verdict(deps, &v, &HashSet::new(), &m, &m);
    assert!(unused.is_empty() && nc.is_empty());
}

#[test]
fn partition_joins_renamed_deps_on_the_resolved_package() {
    // The manifest key is the local alias `md5`; the verdict speaks the real
    // package `md_5` (code form of `md-5`).
    let m = parse_manifest("[dependencies]\nmd5 = { package = \"md-5\", version = \"0.10\" }\n");
    let deps = super::ir::collect_deps(&m, &[]);
    let v = verdict(&[("md_5", DepKind::Normal)], &[]);
    let (unused, _) = super::ir::partition_by_verdict(deps, &v, &HashSet::new(), &m, &m);
    assert_eq!(unused.len(), 1);
    assert_eq!(
        unused[0].original_name, "md5",
        "display name stays the alias"
    );
}

#[test]
fn partition_subtracts_syntactic_credits() {
    let m = parse_manifest("[dependencies]\nmy-doc-dep = \"1\"\n");
    let deps = super::ir::collect_deps(&m, &[]);
    let v = verdict(&[("my_doc_dep", DepKind::Normal)], &[]);
    // A doc-fence credit (separator-stripped form included) clears the finding.
    let syntactic: HashSet<String> = ["mydocdep".to_string()].into();
    let (unused, _) = super::ir::partition_by_verdict(deps, &v, &syntactic, &m, &m);
    assert!(unused.is_empty(), "doc-fence evidence beats the verdict");
}

#[test]
fn delete_consumes_lf_after_dep_line() {
    let m = parse_manifest("[dependencies]\nrand = \"0.8\"\nfoo = \"1\"\n");
    let (s, withheld) =
        super::ir::build_delete_suggestion(&m, &entry(DepSection::Dependencies, "rand")).unwrap();
    let start = s.span.byte_start as usize;
    let end = s.span.byte_end as usize;
    assert_eq!(&m.raw()[start..end], "rand = \"0.8\"\n");
    // The temp manifest lives in a non-repo tempdir: no git backup, so the
    // uniform gate withholds the deletion with the no-repo reason.
    assert!(withheld.unwrap().contains("not in a git repository"));
}

#[test]
fn delete_consumes_crlf_after_dep_line() {
    let m = parse_manifest("[dependencies]\r\nrand = \"0.8\"\r\nfoo = \"1\"\r\n");
    let (s, _) =
        super::ir::build_delete_suggestion(&m, &entry(DepSection::Dependencies, "rand")).unwrap();
    let start = s.span.byte_start as usize;
    let end = s.span.byte_end as usize;
    assert_eq!(&m.raw()[start..end], "rand = \"0.8\"\r\n");
}
