//! Golden-spine tier 2: assembly + union + deps semantics on stable, against
//! hand-crafted fragments. Each fixture encodes one verified spike behavior
//! (cross-crate key join, import discounting, trait-dispatch reachability,
//! synthetic filtering, cfg-union retirement, boundary classification, facade
//! closures) precisely enough that a regression names the broken rule.

use wl_ir::{IrFragment, ItemFact, RefEdge, SCHEMA_VERSION, Span, Visibility};

use super::meta::test_support::fixture_meta;
use super::*;

fn span() -> Option<Span> {
    Some(Span {
        file: "src/lib.rs".into(),
        lo: 0,
        hi: 10,
        from_expansion: false,
    })
}

fn item(path: &[&str], key: &str, kind: &str, parent: Option<&str>) -> ItemFact {
    ItemFact {
        path: path.iter().map(|s| s.to_string()).collect(),
        key: key.into(),
        kind: kind.into(),
        parent_kind: parent.map(String::from),
        trait_item: None,
        visibility: Visibility::Public,
        span: span(),
        vis_span: None,
    }
}

fn edge(from: &[&str], to: &[&str], to_key: &str, import: bool) -> RefEdge {
    RefEdge {
        from: from.iter().map(|s| s.to_string()).collect(),
        to: to.iter().map(|s| s.to_string()).collect(),
        from_key: "fromkey".into(),
        to_key: to_key.into(),
        to_kind: "fn".into(),
        external: from.first() != to.first(),
        import,
    }
}

fn frag(name: &str, items: Vec<ItemFact>, references: Vec<RefEdge>) -> IrFragment {
    IrFragment {
        schema_version: SCHEMA_VERSION,
        crate_name: name.into(),
        items,
        references,
    }
}

fn model(configs: Vec<(&str, Vec<IrFragment>)>) -> SemanticModel {
    SemanticModel::assemble(
        configs
            .into_iter()
            .map(|(id, frags)| (id.to_string(), frags))
            .collect(),
        fixture_meta(),
    )
    .unwrap()
}

fn lead_ids(v: &UnionVerdict) -> Vec<&str> {
    v.leads.iter().map(|l| l.id.as_str()).collect()
}

/// Cross-crate join happens on the stable key, NOT the display path: beta's
/// edge renders alpha's def at a re-export path that string-matches nothing,
/// yet the key join credits the use.
#[test]
fn cross_crate_join_is_by_key_not_path() {
    let alpha = frag(
        "alpha",
        vec![item(&["alpha", "inner", "helper"], "K1", "fn", Some("mod"))],
        vec![],
    );
    let beta = frag(
        "beta",
        vec![],
        // beta observes the def at its re-export path `alpha::helper`.
        vec![edge(&["beta", "main"], &["alpha", "helper"], "K1", false)],
    );
    let m = model(vec![("default", vec![alpha, beta])]);
    assert!(
        lead_ids(&m.union_verdict()).is_empty(),
        "key join must credit the use"
    );
}

/// A `pub use` re-export is not a use-site for unused-pub (it would mask dead
/// names) — but it IS real usage of the defining crate for unused-deps.
#[test]
fn import_edges_are_discounted_for_pub_but_count_for_deps() {
    let alpha = frag(
        "alpha",
        vec![item(&["alpha", "only_reexported"], "K1", "fn", Some("mod"))],
        vec![],
    );
    let beta = frag(
        "beta",
        vec![],
        vec![edge(&["beta"], &["alpha", "only_reexported"], "K1", true)],
    );
    let m = model(vec![("default", vec![alpha, beta])]);
    let v = m.union_verdict();
    assert_eq!(lead_ids(&v), ["alpha::only_reexported"]);
    // …and the lead is in the published lib ⇒ API surface, not dead.
    assert!(!v.leads[0].dead);
}

/// Trait-dispatch reachability: an external-trait impl is a sound root; an
/// internal-trait impl is reached iff its trait method is dispatched; an
/// undispatched internal-trait impl is a lead.
#[test]
fn trait_dispatch_reachability() {
    let mut impl_ext = item(
        &["alpha", "{impl Display}", "fmt"],
        "KE",
        "fn",
        Some("impl"),
    );
    impl_ext.trait_item = Some("EXTERNAL_TRAIT_FMT".into()); // key not in defs

    let trait_decl = item(&["alpha", "Tr", "run"], "KT", "fn", Some("trait"));
    let mut impl_used = item(
        &["alpha", "{impl Tr for A}", "run"],
        "KA",
        "fn",
        Some("impl"),
    );
    impl_used.trait_item = Some("KT".into());
    let mut impl_unused = item(
        &["beta", "{impl Tr for B}", "run"],
        "KB",
        "fn",
        Some("impl"),
    );
    impl_unused.trait_item = Some("KT".into());

    let alpha = frag(
        "alpha",
        vec![impl_ext, trait_decl, impl_used],
        // Somebody dispatches Tr::run generically → edge to the trait method.
        vec![edge(
            &["alpha", "caller"],
            &["alpha", "Tr", "run"],
            "KT",
            false,
        )],
    );
    let beta = frag("beta", vec![impl_unused], vec![]);
    let m = model(vec![("default", vec![alpha, beta])]);

    // KA and KB are both reached via InternalDispatch (the trait method is
    // dispatched → every impl is reachable); KE via ExternalDispatch. The
    // trait *declaration* item (KT) is Category::Other — never a candidate.
    assert!(lead_ids(&m.union_verdict()).is_empty());

    // Remove the dispatch edge: both internal-trait impls become leads; the
    // external-trait impl stays immune.
    let alpha2 = frag(
        "alpha",
        vec![
            {
                let mut i = item(
                    &["alpha", "{impl Display}", "fmt"],
                    "KE",
                    "fn",
                    Some("impl"),
                );
                i.trait_item = Some("EXTERNAL_TRAIT_FMT".into());
                i
            },
            item(&["alpha", "Tr", "run"], "KT", "fn", Some("trait")),
            {
                let mut i = item(
                    &["alpha", "{impl Tr for A}", "run"],
                    "KA",
                    "fn",
                    Some("impl"),
                );
                i.trait_item = Some("KT".into());
                i
            },
        ],
        vec![],
    );
    let m2 = model(vec![("default", vec![alpha2])]);
    assert_eq!(
        lead_ids(&m2.union_verdict()),
        ["alpha::{impl Tr for A}::run"]
    );
}

/// Synthetic defs (no source span — the `--test` harness `main`) are never
/// candidates.
#[test]
fn synthetic_defs_are_not_candidates() {
    let mut synthetic = item(&["alpha", "main"], "KS", "fn", Some("mod"));
    synthetic.span = None;
    let m = model(vec![(
        "default",
        vec![frag("alpha", vec![synthetic], vec![])],
    )]);
    assert!(lead_ids(&m.union_verdict()).is_empty());
}

/// The candidate kinds are widened past the spike's narrowing: an unused pub
/// const is a lead (migration PR 4).
#[test]
fn widened_kinds_include_const() {
    let alpha = frag(
        "alpha",
        vec![item(&["alpha", "LIMIT"], "K1", "const", Some("mod"))],
        vec![],
    );
    let m = model(vec![("default", vec![alpha])]);
    assert_eq!(lead_ids(&m.union_verdict()), ["alpha::LIMIT"]);
}

/// The cfg-matrix union: a primary-config lead used under the `tests` config
/// is retired — even though the `+test` variant carries a different key, the
/// `(crate, def_path)` identity folds them.
#[test]
fn union_retires_test_only_usage() {
    let primary = frag(
        "alpha",
        vec![item(
            &["alpha", "test_helper"],
            "K_PLAIN",
            "fn",
            Some("mod"),
        )],
        vec![],
    );
    // The --tests config re-compiles alpha (different DefPathHash for the same
    // path) and an integration-test crate uses the item.
    let tests_alpha = frag(
        "alpha",
        vec![item(&["alpha", "test_helper"], "K_TEST", "fn", Some("mod"))],
        vec![],
    );
    let tests_it = frag(
        "alpha_it",
        vec![],
        vec![edge(
            &["alpha_it"],
            &["alpha", "test_helper"],
            "K_TEST",
            false,
        )],
    );
    let m = model(vec![
        ("default", vec![primary]),
        ("tests", vec![tests_alpha, tests_it]),
    ]);
    let v = m.union_verdict();
    assert!(lead_ids(&v).is_empty());
    assert_eq!(v.primary_only_leads, 1, "primary alone would over-report");
    assert_eq!(v.retired.len(), 1);
    assert_eq!(v.retired[0].id, "alpha::test_helper");
    assert_eq!(v.retired[0].saved_by, "tests");
}

/// Boundary classification: an unreached pub item in the bin crate is a hard
/// DEAD verdict; in the published lib it's reviewable API surface.
#[test]
fn boundary_splits_dead_from_api_surface() {
    let alpha = frag(
        "alpha",
        vec![item(&["alpha", "surface"], "K1", "fn", Some("mod"))],
        vec![],
    );
    let beta = frag(
        "beta",
        vec![item(&["beta", "dead"], "K2", "fn", Some("mod"))],
        vec![],
    );
    let m = model(vec![("default", vec![alpha, beta])]);
    let v = m.union_verdict();
    let by_id: std::collections::BTreeMap<&str, bool> =
        v.leads.iter().map(|l| (l.id.as_str(), l.dead)).collect();
    assert!(!by_id["alpha::surface"], "published lib ⇒ API surface");
    assert!(by_id["beta::dead"], "bin ⇒ hard verdict");
}

/// Unused-deps: facade closures credit `facade` via a `facade_core` reference;
/// a never-referenced normal dep is flagged; build/optional deps are exempt;
/// dev deps are judged only when a test target compiled.
#[test]
fn deps_verdict_scopes_and_facades() {
    let alpha_edges = vec![edge(
        &["alpha", "user"],
        &["facade_core", "Thing"],
        "K_EXT",
        false,
    )];
    let alpha = frag("alpha", vec![], alpha_edges);

    // Without a test config: dev_helper must be not-judged, not flagged.
    let m = model(vec![("default", vec![alpha.clone()])]);
    let v = m.deps_verdict();
    assert!(!v.dev_deps_judged);
    let alpha_deps = &v.crates[0];
    assert_eq!(alpha_deps.krate, "alpha");
    let unused: Vec<&str> = alpha_deps.unused.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(unused, ["never_used"], "facade credited via closure");
    let not_judged: Vec<(&str, NotJudged)> = alpha_deps
        .not_judged
        .iter()
        .map(|(n, r)| (n.as_str(), *r))
        .collect();
    assert!(not_judged.contains(&("dev_helper", NotJudged::DevWithoutTestConfig)));
    assert!(not_judged.contains(&("hook_installer", NotJudged::BuildDep)));
    assert!(not_judged.contains(&("feature_gated", NotJudged::Optional)));

    // With a --tests config that compiled the integration-test target and
    // exercised dev_helper: judged and exercised.
    let it = frag(
        "alpha_it",
        vec![],
        vec![edge(&["alpha_it"], &["dev_helper", "run"], "K_DEV", false)],
    );
    let m2 = model(vec![
        ("default", vec![alpha.clone()]),
        ("tests", vec![alpha, it]),
    ]);
    let v2 = m2.deps_verdict();
    assert!(v2.dev_deps_judged);
    let unused2: Vec<&str> = v2.crates[0]
        .unused
        .iter()
        .map(|d| d.name.as_str())
        .collect();
    assert_eq!(
        unused2,
        ["never_used"],
        "dev_helper exercised via the test target"
    );
}

/// The loader rejects stale-schema fragments and empty dirs loudly.
#[test]
fn loader_is_strict() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(matches!(
        super::load_fragments(tmp.path()),
        Err(SemanticError::EmptyIrDir { .. })
    ));

    let mut stale = frag("alpha", vec![], vec![]);
    stale.schema_version = 0;
    std::fs::write(
        tmp.path().join("alpha.json"),
        serde_json::to_string(&stale).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        super::load_fragments(tmp.path()),
        Err(SemanticError::BadFragment { .. })
    ));
}
