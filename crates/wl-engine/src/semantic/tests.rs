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
        line: 1,
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
        self_type: None,
        visibility: Visibility::Public,
        span: span(),
        full_span: span(),
        vis_span: None,
        attrs: Vec::new(),
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
        in_signature: false,
        // The fixture edges model `pub use` re-exports wherever import=true.
        reexport: import,
        glob: false,
        alias: None,
        via: None,
        span: None,
        decl_span: None,
        elem_span: None,
    }
}

fn frag(name: &str, items: Vec<ItemFact>, references: Vec<RefEdge>) -> IrFragment {
    IrFragment {
        schema_version: SCHEMA_VERSION,
        crate_name: name.into(),
        target_kind: "lib".into(),
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
/// the lib-rename map credits package `md_5` via an edge to its lib crate
/// `md5`; a never-referenced normal dep is flagged; build/optional deps are
/// exempt; dev deps are judged only when a test target compiled.
#[test]
fn deps_verdict_scopes_and_facades() {
    let alpha_edges = vec![
        edge(
            &["alpha", "user"],
            &["facade_core", "Thing"],
            "K_EXT",
            false,
        ),
        // Edges carry the LIB crate name (`tcx.crate_name`), not the package
        // name — the declared dep is `md_5`, the edge target is `md5`.
        edge(&["alpha", "hasher"], &["md5", "Md5"], "K_MD5", false),
    ];
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

/// Unused-deps: a re-export shim dep is credited through [`RefEdge::via`] —
/// the resolved target defines the item in `std` (a sysroot crate outside
/// every cargo closure), and only the written path root names the dep. An
/// edge without `via` (the pre-`via` fragment shape, exercised through JSON
/// to lock the serde default) must keep the dep flagged.
#[test]
fn deps_verdict_credits_reexport_shim_via_written_root() {
    // Old-shape edge JSON (no `via` key): deserializes with via == None and
    // does NOT credit the shim.
    let old_shape: RefEdge = serde_json::from_str(
        r#"{"from":["alpha","user"],"to":["std","time","Duration"],
            "to_kind":"struct","external":true,"import":true}"#,
    )
    .unwrap();
    assert_eq!(old_shape.via, None);
    let m = model(vec![(
        "default",
        vec![frag("alpha", vec![], vec![old_shape])],
    )]);
    let v = m.deps_verdict();
    let unused: Vec<&str> = v.crates[0].unused.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(
        unused,
        ["facade", "md_5", "never_used"],
        "std edge credits nothing"
    );

    // Same edge with the written root recorded: `never_used` is the shim.
    let mut shimmed = edge(
        &["alpha", "user"],
        &["std", "time", "Duration"],
        "K_STD",
        true,
    );
    shimmed.via = Some("never_used".into());
    let m2 = model(vec![(
        "default",
        vec![frag("alpha", vec![], vec![shimmed])],
    )]);
    let v2 = m2.deps_verdict();
    let unused2: Vec<&str> = v2.crates[0]
        .unused
        .iter()
        .map(|d| d.name.as_str())
        .collect();
    assert_eq!(unused2, ["facade", "md_5"], "via credits the shim dep");
}

/// Build a references-only build-script fragment (what the extractor emits
/// for a `build_script_build` unit).
fn build_frag(references: Vec<RefEdge>) -> IrFragment {
    IrFragment {
        schema_version: SCHEMA_VERSION,
        crate_name: "build_script_build".into(),
        target_kind: "build".into(),
        items: Vec::new(),
        references,
    }
}

/// Build-fragment edges join by DISPLAY PATH, not key: the build script's
/// deps compile in Build mode, whose `-Cmetadata` (hence `DefPathHash`
/// generation) differs from the Check-mode units the defs came from. A
/// path-joined use retires the lead; an edge to a path nothing defines is a
/// no-op (never a false join); and the phantom `build_script_build` crate
/// name stays out of the assembly's member set.
#[test]
fn build_fragment_edges_join_by_path_fallback() {
    let alpha = frag(
        "alpha",
        vec![item(&["alpha", "helper"], "K_CHECK", "fn", Some("mod"))],
        vec![],
    );
    // Same def, referenced from a build script under its Build-mode key.
    let build = build_frag(vec![
        edge(
            &["build_script_build", "main"],
            &["alpha", "helper"],
            "K_BUILD_MODE",
            false,
        ),
        edge(
            &["build_script_build", "main"],
            &["alpha", "nonexistent"],
            "K_NOWHERE",
            false,
        ),
    ]);
    let m = model(vec![("default", vec![alpha.clone(), build])]);
    assert!(
        lead_ids(&m.union_verdict()).is_empty(),
        "build.rs use must retire the lead via the path join"
    );

    // Without the build fragment the same def is a lead (the control).
    let m2 = model(vec![("default", vec![alpha])]);
    assert_eq!(lead_ids(&m2.union_verdict()), ["alpha::helper"]);
}

/// Build fragments are excluded from the architecture substrate
/// (`references_from`) and never credit `[dependencies]` (`DepUsage` finds no
/// owner for `build_script_build` — build-deps stay unjudged).
#[test]
fn build_fragments_stay_out_of_architecture_and_deps() {
    let alpha = frag(
        "alpha",
        vec![item(&["alpha", "helper"], "K1", "fn", Some("mod"))],
        vec![],
    );
    let build = build_frag(vec![edge(
        &["build_script_build", "main"],
        &["facade_core", "Thing"],
        "K_EXT",
        false,
    )]);
    let m = model(vec![("default", vec![alpha, build])]);
    // Architecture: no crate is named `build_script_build`, and even asking
    // for it returns nothing (the target-kind filter).
    assert!(m.references_from("build_script_build").is_empty());
    // Deps: the facade edge lives ONLY in the ownerless build fragment, so
    // `facade` stays unused — a build.rs use must not credit [dependencies].
    let v = m.deps_verdict();
    let unused: Vec<&str> = v.crates[0].unused.iter().map(|d| d.name.as_str()).collect();
    assert!(unused.contains(&"facade"), "{unused:?}");
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

/// (PR 9) An export-shaped attribute roots a def: the FFI export with no Rust
/// referrer is NOT a lead — the `ffi_no_mangle_export` false positive retired.
#[test]
fn export_attrs_root_reachability() {
    let mut ffi = item(&["alpha", "ffi_export"], "K1", "fn", Some("mod"));
    ffi.attrs = vec!["no_mangle".into()];
    let plain = item(&["alpha", "plain_dead"], "K2", "fn", Some("mod"));
    let m = model(vec![(
        "default",
        vec![frag("alpha", vec![ffi, plain], vec![])],
    )]);
    assert_eq!(
        lead_ids(&m.union_verdict()),
        ["alpha::plain_dead"],
        "the export-rooted fn must not be a lead; the plain one still is"
    );
}

/// (PR 9) Signature exposure: a def named in a PUB item's signature is
/// flagged; one named only in a private item's signature is not.
#[test]
fn signature_exposure_requires_pub_from() {
    let exposed = item(&["alpha", "Exposed"], "K_EXPOSED", "struct", Some("mod"));
    let hidden = item(&["alpha", "Hidden"], "K_HIDDEN", "struct", Some("mod"));
    let pub_fn = item(&["alpha", "api"], "K_API", "fn", Some("mod"));
    let mut priv_fn = item(&["alpha", "internal"], "K_INT", "fn", Some("mod"));
    priv_fn.visibility = Visibility::Restricted("crate".into());

    let mut sig_edge = edge(&["alpha", "api"], &["alpha", "Exposed"], "K_EXPOSED", false);
    sig_edge.from_key = "K_API".into();
    sig_edge.in_signature = true;
    let mut priv_sig_edge = edge(
        &["alpha", "internal"],
        &["alpha", "Hidden"],
        "K_HIDDEN",
        false,
    );
    priv_sig_edge.from_key = "K_INT".into();
    priv_sig_edge.in_signature = true;

    let m = model(vec![(
        "default",
        vec![frag(
            "alpha",
            vec![exposed, hidden, pub_fn, priv_fn],
            vec![sig_edge, priv_sig_edge],
        )],
    )]);
    let asm = m.primary();
    assert!(asm.exposed_in_public_signature("K_EXPOSED"));
    assert!(!asm.exposed_in_public_signature("K_HIDDEN"));
}

/// (PR 9) Pub-module-hop reachability: pub def under pub modules is
/// externally reachable; the same def under a private module is not; a
/// non-module path segment (impl rendering) is transparent.
#[test]
fn external_reachability_walks_module_visibility() {
    let mut pub_mod = item(&["alpha", "api"], "K_MOD_PUB", "mod", Some("mod"));
    pub_mod.kind = "mod".into();
    let mut priv_mod = item(&["alpha", "detail"], "K_MOD_PRIV", "mod", Some("mod"));
    priv_mod.kind = "mod".into();
    priv_mod.visibility = Visibility::Restricted("crate".into());

    let reachable = item(&["alpha", "api", "f"], "K_R", "fn", Some("mod"));
    let unreachable = item(&["alpha", "detail", "g"], "K_U", "fn", Some("mod"));

    let m = model(vec![(
        "default",
        vec![frag(
            "alpha",
            vec![pub_mod, priv_mod, reachable, unreachable],
            vec![],
        )],
    )]);
    let asm = m.primary();
    let def = |k: &str| asm.defs.get(k).unwrap();
    assert!(asm.is_externally_reachable("K_R", def("K_R")));
    assert!(!asm.is_externally_reachable("K_U", def("K_U")));
}

/// (PR 10) The per-candidate classification the ported unused-pub lint
/// consumes: cross-vs-intra-vs-unused mirrors the syn `Usage` vocabulary —
/// a foreign-crate use-site wins over an intra one, intra over none.
#[test]
fn pub_candidates_split_cross_intra_unused() {
    let alpha = frag(
        "alpha",
        vec![
            item(&["alpha", "cross_used"], "K_X", "fn", Some("mod")),
            item(&["alpha", "intra_used"], "K_I", "fn", Some("mod")),
            item(&["alpha", "dead"], "K_D", "fn", Some("mod")),
        ],
        vec![edge(
            &["alpha", "caller"],
            &["alpha", "intra_used"],
            "K_I",
            false,
        )],
    );
    let beta = frag(
        "beta",
        vec![],
        vec![edge(
            &["beta", "main"],
            &["alpha", "cross_used"],
            "K_X",
            false,
        )],
    );
    let m = model(vec![("default", vec![alpha, beta])]);
    let usage_of = |id: &str| {
        m.pub_candidates()
            .into_iter()
            .find(|c| c.id == id)
            .map(|c| c.usage)
            .unwrap()
    };
    assert_eq!(usage_of("alpha::cross_used"), PubUsage::CrossCrate);
    assert_eq!(usage_of("alpha::intra_used"), PubUsage::IntraCrate);
    assert_eq!(usage_of("alpha::dead"), PubUsage::Unused);
}

/// (PR 10) The union is per-candidate too: an item reached only under the
/// `--tests` config reads IntraCrate (not Unused), and a cross-crate use in
/// ANY config wins over intra usage in the primary.
#[test]
fn pub_candidates_union_across_configs() {
    let alpha_default = frag(
        "alpha",
        vec![
            item(&["alpha", "test_only"], "K_T", "fn", Some("mod")),
            item(&["alpha", "intra_then_cross"], "K_C", "fn", Some("mod")),
        ],
        vec![edge(
            &["alpha", "caller"],
            &["alpha", "intra_then_cross"],
            "K_C",
            false,
        )],
    );
    // Under --tests the same defs carry different keys (StableCrateId moves)
    // but the same identity path; the unit-test module calls test_only, and
    // an integration-test crate (a NON-member) calls intra_then_cross.
    let alpha_tests = frag(
        "alpha",
        vec![
            item(&["alpha", "test_only"], "K_T2", "fn", Some("mod")),
            item(&["alpha", "intra_then_cross"], "K_C2", "fn", Some("mod")),
        ],
        vec![edge(
            &["alpha", "tests", "calls_it"],
            &["alpha", "test_only"],
            "K_T2",
            false,
        )],
    );
    let it_crate = frag(
        "it_case",
        vec![],
        vec![edge(
            &["it_case", "t"],
            &["alpha", "intra_then_cross"],
            "K_C2",
            false,
        )],
    );
    let m = model(vec![
        ("default", vec![alpha_default]),
        ("--tests", vec![alpha_tests, it_crate]),
    ]);
    let usage_of = |id: &str| {
        m.pub_candidates()
            .into_iter()
            .find(|c| c.id == id)
            .map(|c| c.usage)
            .unwrap()
    };
    assert_eq!(usage_of("alpha::test_only"), PubUsage::IntraCrate);
    assert_eq!(usage_of("alpha::intra_then_cross"), PubUsage::CrossCrate);
    // The integration-test crate is not a primary member: none of its defs
    // may become candidates.
    assert!(m.pub_candidates().iter().all(|c| c.krate == "alpha"));
}

// --- the one-pass unused-pub `--fix` cascade (`pub_candidates_excluding` /
//     `dangling_imports`) ---

/// A plain `use`-declaration leaf carrying the `--fix` deletion spans. `braced`
/// picks the extractor's discriminator: a brace-list leaf has `decl == elem`
/// (rustc collapses the leaf item's span); a standalone `use a::b;` has `decl`
/// strictly containing `elem`. `lo` keeps distinct imports from colliding on
/// the `(file, decl.lo, elem.lo)` dedup key.
fn import_edge(from: &[&str], to: &[&str], to_key: &str, braced: bool, lo: u32) -> RefEdge {
    let elem = Span {
        file: "src/lib.rs".into(),
        lo,
        hi: lo + 10,
        line: 5,
        from_expansion: false,
    };
    let decl = if braced {
        elem.clone()
    } else {
        Span {
            lo: lo.saturating_sub(4),
            hi: lo + 12,
            ..elem.clone()
        }
    };
    RefEdge {
        reexport: false,
        decl_span: Some(decl),
        elem_span: Some(elem),
        ..edge(from, to, to_key, true)
    }
}

fn usage_map(cands: &[PubCandidate]) -> std::collections::HashMap<String, PubUsage> {
    cands.iter().map(|c| (c.id.clone(), c.usage)).collect()
}

/// The core cascade: an item reached only by a now-removed item drops to
/// `Unused` in the same pass, so `--fix` deletes the whole dead chain in one
/// run instead of one layer per invocation.
#[test]
fn cascade_frees_transitively_dead_item() {
    let app = frag(
        "app",
        vec![
            item(&["app", "driver"], "K_DRV", "fn", Some("mod")),
            item(&["app", "helper"], "K_HLP", "fn", Some("mod")),
        ],
        vec![edge(&["app", "driver"], &["app", "helper"], "K_HLP", false)],
    );
    let m = model(vec![("default", vec![app])]);
    let base = usage_map(&m.pub_candidates());
    assert_eq!(base["app::driver"], PubUsage::Unused);
    assert_eq!(base["app::helper"], PubUsage::IntraCrate);

    let removed = RemovalSet::new(["app::driver"]);
    let after = usage_map(&m.pub_candidates_excluding(&removed));
    assert_eq!(
        after["app::helper"],
        PubUsage::Unused,
        "helper, reached only by the removed driver, must free"
    );
}

/// From-attribution is segment-wise: removing `app::driver` drops edges out of
/// `driver` and its body-nested defs (`driver::{closure}`) but NOT a sibling
/// whose path merely shares the string prefix (`driver_two`).
#[test]
fn cascade_from_attribution_is_segment_prefix() {
    let app = frag(
        "app",
        vec![
            item(&["app", "driver"], "K_DRV", "fn", Some("mod")),
            item(&["app", "driver_two"], "K_DRV2", "fn", Some("mod")),
            item(&["app", "helper"], "K_HLP", "fn", Some("mod")),
            item(&["app", "keeper"], "K_KP", "fn", Some("mod")),
        ],
        vec![
            // helper reached ONLY from a closure nested inside driver.
            edge(
                &["app", "driver", "{closure#0}"],
                &["app", "helper"],
                "K_HLP",
                false,
            ),
            // keeper reached from the sibling driver_two.
            edge(&["app", "driver_two"], &["app", "keeper"], "K_KP", false),
        ],
    );
    let m = model(vec![("default", vec![app])]);
    let after = usage_map(&m.pub_candidates_excluding(&RemovalSet::new(["app::driver"])));
    assert_eq!(
        after["app::helper"],
        PubUsage::Unused,
        "driver's closure edge must drop with driver"
    );
    assert_eq!(
        after["app::keeper"],
        PubUsage::IntraCrate,
        "driver_two is not segment-covered by `driver` — its edge survives"
    );
}

/// The cascade preserves the cfg-matrix union: an item that becomes `Unused`
/// in one config after removal but is still used under ANOTHER config stays
/// alive — deleting it would break that config's build.
#[test]
fn cascade_preserves_cross_config_union() {
    let default_cfg = frag(
        "app",
        vec![
            item(&["app", "driver"], "K_DRV", "fn", Some("mod")),
            item(&["app", "helper"], "K_HLP", "fn", Some("mod")),
        ],
        vec![edge(&["app", "driver"], &["app", "helper"], "K_HLP", false)],
    );
    // Under `--tests` helper is reached by a test-only keeper (distinct key,
    // same identity path — StableCrateId moves across configs).
    let tests_cfg = frag(
        "app",
        vec![
            item(&["app", "driver"], "K_DRV2", "fn", Some("mod")),
            item(&["app", "helper"], "K_HLP2", "fn", Some("mod")),
        ],
        vec![edge(
            &["app", "tests", "keeper"],
            &["app", "helper"],
            "K_HLP2",
            false,
        )],
    );
    let m = model(vec![
        ("default", vec![default_cfg]),
        ("--tests", vec![tests_cfg]),
    ]);
    let after = usage_map(&m.pub_candidates_excluding(&RemovalSet::new(["app::driver"])));
    assert_eq!(
        after["app::helper"],
        PubUsage::IntraCrate,
        "helper is Unused in default-after-removal but used under --tests → union keeps it"
    );
}

/// `dangling_imports` returns exactly the `use` leaves whose target is removed,
/// deduped across configs, carrying the brace discriminator (`decl == elem`).
#[test]
fn dangling_imports_targets_removed_defs_only() {
    let app = frag(
        "app",
        vec![
            item(&["app", "inner", "helper"], "K_HLP", "fn", Some("mod")),
            item(&["app", "inner", "kept"], "K_KEPT", "fn", Some("mod")),
        ],
        vec![
            import_edge(&["app"], &["app", "inner", "helper"], "K_HLP", true, 100),
            import_edge(&["app"], &["app", "inner", "kept"], "K_KEPT", true, 200),
        ],
    );
    let m = model(vec![("default", vec![app])]);
    let dangling = m.dangling_imports(&RemovalSet::new(["app::inner::helper"]));
    assert_eq!(dangling.len(), 1, "only the helper import dangles");
    let d = &dangling[0];
    assert_eq!(d.elem.lo, 100, "the helper leaf, not the kept one");
    assert!(
        d.decl.lo == d.elem.lo && d.decl.hi == d.elem.hi,
        "brace-leaf: decl == elem (the excise discriminator)"
    );
    assert!(!d.reexport);
}

/// (PR 10) The must-stay-pub guards surface per candidate: a `use`/`pub use`
/// target is flagged `reexport_target` (usage still Unused — imports are
/// discounted), and an export-shaped attr classifies DispatchReached (the
/// `ffi_no_mangle_export` fix).
#[test]
fn pub_candidates_guards_and_export_roots() {
    let mut ffi = item(&["alpha", "ffi_export"], "K_F", "fn", Some("mod"));
    ffi.attrs = vec!["no_mangle".into()];
    let mut vised = item(&["alpha", "reexported"], "K_R", "fn", Some("mod"));
    vised.vis_span = Some(Span {
        file: "src/lib.rs".into(),
        lo: 0,
        hi: 3,
        line: 1,
        from_expansion: false,
    });
    let alpha = frag("alpha", vec![ffi, vised], vec![]);
    let beta = frag(
        "beta",
        vec![],
        vec![edge(&["beta"], &["alpha", "reexported"], "K_R", true)],
    );
    let m = model(vec![("default", vec![alpha, beta])]);
    let cand = |id: &str| m.pub_candidates().into_iter().find(|c| c.id == id).unwrap();
    let ffi = cand("alpha::ffi_export");
    assert_eq!(ffi.usage, PubUsage::DispatchReached);
    let re = cand("alpha::reexported");
    assert!(re.reexport_target);
    assert_eq!(re.usage, PubUsage::Unused);
    // Spans ride along for the lint's anchor + tighten fix.
    assert_eq!(re.vis_span.as_ref().map(|s| (s.lo, s.hi)), Some((0, 3)));
    assert_eq!(re.name, "reexported");
    assert_eq!(re.kind, "fn");
}

// --- the cross-config global hash join (a `+test`/bench/integration edge to a
//     dependency's PLAIN rlib, whose def the referring config never extracted) ---

/// The regression this fix exists for: a member's `+test` unit calls a
/// sibling's plain def, but under `--tests` the sibling's plain lib is
/// cargo-fresh — its fragment lives only in the primary dir, so the edge
/// carries the plain-generation key the tests config never extracted. The
/// per-config join drops it (reads `Unused`); the global index resolves it
/// (translating to the tests config's own key for the identity), crediting the
/// cross-crate use, and the union credits the config that proved it.
#[test]
fn cross_config_global_join_resolves_test_only_use() {
    // default: alpha's plain lib (K_PLAIN) + beta (a member, no test code yet).
    let alpha_default = frag(
        "alpha",
        vec![item(&["alpha", "render_one"], "K_PLAIN", "fn", Some("mod"))],
        vec![],
    );
    let beta_default = frag("beta", vec![], vec![]);
    // --tests: alpha recompiled as its own test harness (a DIFFERENT key for
    // the same identity), and beta's test code calls `alpha::render_one` — but
    // the edge targets alpha's PLAIN key (test units link plain rlibs), which
    // the tests dir never extracted.
    let alpha_tests = frag(
        "alpha",
        vec![item(
            &["alpha", "render_one"],
            "K_TESTGEN",
            "fn",
            Some("mod"),
        )],
        vec![],
    );
    let beta_tests = frag(
        "beta",
        vec![],
        vec![edge(
            &["beta", "tests", "snapshot"],
            &["alpha", "render_one"],
            "K_PLAIN",
            false,
        )],
    );
    let m = model(vec![
        ("default", vec![alpha_default, beta_default]),
        ("tests", vec![alpha_tests, beta_tests]),
    ]);
    let usage_of = |id: &str| {
        m.pub_candidates()
            .into_iter()
            .find(|c| c.id == id)
            .map(|c| c.usage)
            .unwrap()
    };
    assert_eq!(
        usage_of("alpha::render_one"),
        PubUsage::CrossCrate,
        "the plain-generation test edge must resolve via the global join"
    );
    // The union retires the primary-config lead and credits `tests`.
    let v = m.union_verdict();
    assert!(lead_ids(&v).is_empty());
    assert_eq!(v.retired.len(), 1);
    assert_eq!(v.retired[0].id, "alpha::render_one");
    assert_eq!(v.retired[0].saved_by, "tests");
}

/// Same global join, but the referrer is an integration-test crate (a
/// non-member): its edge to a member's plain def resolves cross-crate, and it
/// contributes no candidate of its own.
#[test]
fn cross_config_global_join_credits_integration_test_crate() {
    let alpha_default = frag(
        "alpha",
        vec![item(&["alpha", "helper"], "K_PLAIN", "fn", Some("mod"))],
        vec![],
    );
    let alpha_tests = frag(
        "alpha",
        vec![item(&["alpha", "helper"], "K_TESTGEN", "fn", Some("mod"))],
        vec![],
    );
    let it_crate = frag(
        "alpha_it",
        vec![],
        vec![edge(&["alpha_it"], &["alpha", "helper"], "K_PLAIN", false)],
    );
    let m = model(vec![
        ("default", vec![alpha_default]),
        ("tests", vec![alpha_tests, it_crate]),
    ]);
    let cands = m.pub_candidates();
    let usage = cands
        .iter()
        .find(|c| c.id == "alpha::helper")
        .map(|c| c.usage)
        .unwrap();
    assert_eq!(usage, PubUsage::CrossCrate);
    assert!(
        cands.iter().all(|c| c.krate == "alpha"),
        "the integration-test crate is a non-member: no candidate of its own"
    );
}

/// The cascade inherits the global join: a cross-config use keeps a def alive
/// until its (test) referrer is removed, then it frees; an unrelated removal
/// leaves the credit intact.
#[test]
fn cascade_respects_cross_config_global_join() {
    let alpha_default = frag(
        "alpha",
        vec![item(&["alpha", "helper"], "K_PLAIN", "fn", Some("mod"))],
        vec![],
    );
    let beta_default = frag(
        "beta",
        vec![item(&["beta", "driver"], "K_DRV", "fn", Some("mod"))],
        vec![],
    );
    let alpha_tests = frag(
        "alpha",
        vec![item(&["alpha", "helper"], "K_TESTGEN", "fn", Some("mod"))],
        vec![],
    );
    // beta's test code calls alpha::helper via the plain (default-gen) key.
    let beta_tests = frag(
        "beta",
        vec![item(&["beta", "driver"], "K_DRV2", "fn", Some("mod"))],
        vec![edge(
            &["beta", "driver"],
            &["alpha", "helper"],
            "K_PLAIN",
            false,
        )],
    );
    let m = model(vec![
        ("default", vec![alpha_default, beta_default]),
        ("tests", vec![alpha_tests, beta_tests]),
    ]);
    assert_eq!(
        usage_map(&m.pub_candidates())["alpha::helper"],
        PubUsage::CrossCrate
    );
    // Removing the cross-config referrer frees helper (the refold re-runs the
    // global join, then drops the removed item's edges).
    let after = usage_map(&m.pub_candidates_excluding(&RemovalSet::new(["beta::driver"])));
    assert_eq!(
        after["alpha::helper"],
        PubUsage::Unused,
        "helper freed once its only (test) referrer is removed"
    );
    // An unrelated removal keeps the global-join credit.
    let after2 = usage_map(&m.pub_candidates_excluding(&RemovalSet::new(["alpha::other"])));
    assert_eq!(after2["alpha::helper"], PubUsage::CrossCrate);
}

/// The foreign-reach channel: a config can reference a crate it never
/// extracted at all — `[lib] bench = false` means `--benches` compiles only
/// the bench target (the plain lib is cargo-fresh), so the benches dir holds
/// NO fragment of the defining crate and the global join has no landing key.
/// The reach is credited at identity level instead, classifying CrossCrate
/// and retiring the union lead with correct attribution.
#[test]
fn foreign_reach_credits_unextracted_target_crate() {
    let alpha_default = frag(
        "alpha",
        vec![item(&["alpha", "measured"], "K_PLAIN", "fn", Some("mod"))],
        vec![],
    );
    // The benches config: ONLY the bench crate's fragment — no alpha at all.
    let bench = frag(
        "lookup",
        vec![],
        vec![edge(
            &["lookup", "main"],
            &["alpha", "measured"],
            "K_PLAIN",
            false,
        )],
    );
    let m = model(vec![
        ("default", vec![alpha_default]),
        ("benches", vec![bench]),
    ]);
    let usage = m
        .pub_candidates()
        .into_iter()
        .find(|c| c.id == "alpha::measured")
        .map(|c| c.usage)
        .unwrap();
    assert_eq!(
        usage,
        PubUsage::CrossCrate,
        "the bench edge must credit the identity despite no local def"
    );
    let v = m.union_verdict();
    assert!(lead_ids(&v).is_empty());
    assert_eq!(v.retired.len(), 1);
    assert_eq!(v.retired[0].id, "alpha::measured");
    assert_eq!(v.retired[0].saved_by, "benches");
}

/// The cascade recomputes foreign reach: removing the foreign referrer frees
/// the target in the same pass; an unrelated removal keeps the credit.
#[test]
fn cascade_recomputes_foreign_reach() {
    let alpha_default = frag(
        "alpha",
        vec![item(&["alpha", "measured"], "K_PLAIN", "fn", Some("mod"))],
        vec![],
    );
    let bench = frag(
        "lookup",
        vec![],
        vec![edge(
            &["lookup", "runner"],
            &["alpha", "measured"],
            "K_PLAIN",
            false,
        )],
    );
    let m = model(vec![
        ("default", vec![alpha_default]),
        ("benches", vec![bench]),
    ]);
    let after = usage_map(&m.pub_candidates_excluding(&RemovalSet::new(["lookup::runner"])));
    assert_eq!(
        after["alpha::measured"],
        PubUsage::Unused,
        "freed once its only (foreign) referrer is removed"
    );
    let after2 = usage_map(&m.pub_candidates_excluding(&RemovalSet::new(["alpha::other"])));
    assert_eq!(after2["alpha::measured"], PubUsage::CrossCrate);
}

/// Import surgery sees a `use` of a removed item even from a config that
/// never extracted the defining crate (`target_identity`'s foreign branch).
#[test]
fn dangling_imports_resolve_foreign_config() {
    let alpha_default = frag(
        "alpha",
        vec![item(&["alpha", "measured"], "K_PLAIN", "fn", Some("mod"))],
        vec![],
    );
    let bench = frag(
        "lookup",
        vec![],
        vec![import_edge(
            &["lookup"],
            &["alpha", "measured"],
            "K_PLAIN",
            false,
            300,
        )],
    );
    let m = model(vec![
        ("default", vec![alpha_default]),
        ("benches", vec![bench]),
    ]);
    let dangling = m.dangling_imports(&RemovalSet::new(["alpha::measured"]));
    assert_eq!(dangling.len(), 1, "the foreign-config import must dangle");
    assert_eq!(dangling[0].elem.lo, 300);
}

/// `def_for_edge` (the import-surgery substrate) resolves a `use` in a `+test`
/// unit naming a sibling's plain def, so removing that def surfaces the import
/// as dangling. (Here the target's `+test` fragment is in the tests config, so
/// the display-path fallback also covers it; the value is guarding that the
/// cross-config test import is found at all.)
#[test]
fn dangling_imports_resolve_cross_config() {
    let alpha_default = frag(
        "alpha",
        vec![item(&["alpha", "helper"], "K_PLAIN", "fn", Some("mod"))],
        vec![],
    );
    let alpha_tests = frag(
        "alpha",
        vec![item(&["alpha", "helper"], "K_TESTGEN", "fn", Some("mod"))],
        vec![],
    );
    // beta's test unit: `use alpha::helper;` written against the plain key.
    let beta_tests = frag(
        "beta",
        vec![],
        vec![import_edge(
            &["beta", "tests"],
            &["alpha", "helper"],
            "K_PLAIN",
            false,
            100,
        )],
    );
    let m = model(vec![
        ("default", vec![alpha_default]),
        ("tests", vec![alpha_tests, beta_tests]),
    ]);
    let dangling = m.dangling_imports(&RemovalSet::new(["alpha::helper"]));
    assert_eq!(
        dangling.len(),
        1,
        "the cross-config test import must resolve and dangle"
    );
    assert_eq!(dangling[0].elem.lo, 100);
}
