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
        self_kind: None,
        self_copy: None,
    }
}

fn edge(from: &[&str], to: &[&str], to_key: &str, import: bool) -> RefEdge {
    // Name-derived `external` models the common case; sibling-target edges
    // (same crate name, different crate) set it explicitly via `edge_ext`.
    edge_ext(from, to, to_key, import, from.first() != to.first())
}

fn edge_ext(from: &[&str], to: &[&str], to_key: &str, import: bool, external: bool) -> RefEdge {
    RefEdge {
        from: from.iter().map(|s| s.to_string()).collect(),
        // Empty models a pre-6 fragment (the textual scope walk). Fixtures
        // with clean module-chain from-paths resolve identically either way;
        // tests for the lexical-module semantics set it via `with_module`.
        from_module: Vec::new(),
        to: to.iter().map(|s| s.to_string()).collect(),
        from_key: "fromkey".into(),
        to_key: to_key.into(),
        to_kind: "fn".into(),
        external,
        import,
        in_signature: false,
        receiver_resolved: false,
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
    frag_target(name, "lib", items, references)
}

fn frag_target(
    name: &str,
    target_kind: &str,
    items: Vec<ItemFact>,
    references: Vec<RefEdge>,
) -> IrFragment {
    IrFragment {
        schema_version: SCHEMA_VERSION,
        crate_name: name.into(),
        target_kind: target_kind.into(),
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

/// A same-package sibling target (bin `main.rs`, integration test) shares the
/// lib's crate NAME but compiles as its own crate — `pub(crate)` cannot reach
/// it. The visibility split must trust the extractor's `CrateNum` comparison
/// (`RefEdge::external`), not the name (the `app_leave_dates::App` narrowing
/// regression from the 2026-07-05 LeaveDates validation).
#[test]
fn sibling_bin_target_use_is_cross_crate() {
    let lib = frag(
        "alpha",
        vec![item(&["alpha", "App"], "K_APP", "fn", Some("mod"))],
        vec![],
    );
    // The package's own bin: same crate name, `external: true` (different
    // CrateNum in the extractor's universe).
    let bin = frag_target(
        "alpha",
        "bin",
        vec![],
        vec![edge_ext(
            &["alpha", "main"],
            &["alpha", "App"],
            "K_APP",
            false,
            true,
        )],
    );
    let m = model(vec![("default", vec![lib, bin])]);
    let usage = m
        .pub_candidates()
        .into_iter()
        .find(|c| c.id == "alpha::App")
        .map(|c| c.usage)
        .unwrap();
    assert_eq!(
        usage,
        PubUsage::CrossCrate,
        "a bin→lib edge within one package must keep the item pub"
    );
}

/// An extension trait imported cross-crate purely for method-call syntax has
/// no direct edge to the trait def (the import edge is discounted; the call
/// edges land on the trait *members*) — member reach must fold onto the
/// owning trait declaration (the `StrExt`/`EventTargetExt` narrowing
/// regression from the 2026-07-05 LeaveDates validation).
#[test]
fn trait_member_use_credits_parent_trait() {
    let alpha = frag(
        "alpha",
        vec![
            item(&["alpha", "StrExt"], "K_TR", "trait", Some("mod")),
            item(&["alpha", "StrExt", "shout"], "K_M", "fn", Some("trait")),
        ],
        vec![],
    );
    let beta = frag(
        "beta",
        vec![],
        vec![
            // `use alpha::StrExt;` — discounted as an import…
            edge(&["beta"], &["alpha", "StrExt"], "K_TR", true),
            // …but the method call is a real cross-crate use of the member,
            // and the trait must stay `pub` for it to compile.
            edge(
                &["beta", "caller"],
                &["alpha", "StrExt", "shout"],
                "K_M",
                false,
            ),
        ],
    );
    let m = model(vec![("default", vec![alpha, beta])]);
    let usage = m
        .pub_candidates()
        .into_iter()
        .find(|c| c.id == "alpha::StrExt")
        .map(|c| c.usage)
        .unwrap();
    assert_eq!(usage, PubUsage::CrossCrate);
}

/// Method calls can also resolve to the IMPL's method (whose `trait_item`
/// names the decl) rather than the trait-decl method — the trait is credited
/// through that linkage too.
#[test]
fn trait_impl_method_use_credits_parent_trait() {
    let mut impl_method = item(
        &["alpha", "{impl StrExt for &str}", "shout"],
        "K_IM",
        "fn",
        Some("impl"),
    );
    impl_method.trait_item = Some("K_M".into());
    let alpha = frag(
        "alpha",
        vec![
            item(&["alpha", "StrExt"], "K_TR", "trait", Some("mod")),
            item(&["alpha", "StrExt", "shout"], "K_M", "fn", Some("trait")),
            impl_method,
        ],
        vec![],
    );
    let beta = frag(
        "beta",
        vec![],
        vec![edge(
            &["beta", "caller"],
            &["alpha", "{impl StrExt for &str}", "shout"],
            "K_IM",
            false,
        )],
    );
    let m = model(vec![("default", vec![alpha, beta])]);
    let usage = m
        .pub_candidates()
        .into_iter()
        .find(|c| c.id == "alpha::StrExt")
        .map(|c| c.usage)
        .unwrap();
    assert_eq!(usage, PubUsage::CrossCrate);
}

/// A trait declaring a member nothing calls is flagged `dead_members`:
/// narrowing it un-exempts the trait from rustc `dead_code`, which then
/// flags the member — the fix must not machine-apply.
#[test]
fn trait_with_uncalled_member_is_flagged_dead_members() {
    let alpha = frag(
        "alpha",
        vec![
            item(&["alpha", "Ext"], "K_TR", "trait", Some("mod")),
            item(&["alpha", "Ext", "used"], "K_U", "fn", Some("trait")),
            item(
                &["alpha", "Ext", "never_called"],
                "K_D",
                "fn",
                Some("trait"),
            ),
        ],
        vec![edge(
            &["alpha", "caller"],
            &["alpha", "Ext", "used"],
            "K_U",
            false,
        )],
    );
    let m = model(vec![("default", vec![alpha])]);
    let cand = m
        .pub_candidates()
        .into_iter()
        .find(|c| c.id == "alpha::Ext")
        .unwrap();
    assert_eq!(cand.usage, PubUsage::IntraCrate);
    assert!(cand.dead_members, "never_called has no reaching edge");

    // Every member reached → narrowing is clean.
    let alpha_ok = frag(
        "alpha",
        vec![
            item(&["alpha", "Ext"], "K_TR", "trait", Some("mod")),
            item(&["alpha", "Ext", "used"], "K_U", "fn", Some("trait")),
        ],
        vec![edge(
            &["alpha", "caller"],
            &["alpha", "Ext", "used"],
            "K_U",
            false,
        )],
    );
    let m = model(vec![("default", vec![alpha_ok])]);
    let cand = m
        .pub_candidates()
        .into_iter()
        .find(|c| c.id == "alpha::Ext")
        .unwrap();
    assert!(!cand.dead_members);
}

/// The same fold keeps an intra-only method call intra: a trait whose members
/// are called only inside the defining crate still tightens.
#[test]
fn trait_member_intra_use_stays_intra() {
    let alpha = frag(
        "alpha",
        vec![
            item(&["alpha", "StrExt"], "K_TR", "trait", Some("mod")),
            item(&["alpha", "StrExt", "shout"], "K_M", "fn", Some("trait")),
        ],
        vec![edge(
            &["alpha", "caller"],
            &["alpha", "StrExt", "shout"],
            "K_M",
            false,
        )],
    );
    let m = model(vec![("default", vec![alpha])]);
    let usage = m
        .pub_candidates()
        .into_iter()
        .find(|c| c.id == "alpha::StrExt")
        .map(|c| c.usage)
        .unwrap();
    assert_eq!(usage, PubUsage::IntraCrate);
}

/// A proc-macro entry point (`#[proc_macro_derive]` — emitted as a
/// `proc_macro` export attr) is public API by construction: the compiler-
/// synthesized `_DECLS` registration gives it a phantom intra-crate edge, and
/// narrowing it is a hard compile error. Export roots must win over Direct.
#[test]
fn proc_macro_entry_is_never_a_lead() {
    let mut entry = item(&["macros", "derive_x"], "K_P", "fn", Some("mod"));
    entry.attrs = vec!["proc_macro".into()];
    let macros = frag_target(
        "macros",
        "proc-macro",
        vec![entry],
        // The synthesized registration edge — intra-crate, real (non-import).
        vec![edge(
            &["macros", "_", "_DECLS"],
            &["macros", "derive_x"],
            "K_P",
            false,
        )],
    );
    let m = model(vec![("default", vec![macros])]);
    let usage = m
        .pub_candidates()
        .into_iter()
        .find(|c| c.id == "macros::derive_x")
        .map(|c| c.usage)
        .unwrap();
    assert_eq!(
        usage,
        PubUsage::DispatchReached,
        "the _DECLS edge must not read as intra-crate usage"
    );
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

/// An item whose every use-site is test-cfg-gated (`IntraCrate` reached only
/// outside the primary config) is flagged `test_only`: narrowing it compiles
/// but leaves it `dead_code` on the plain build, so `--fix` must not apply.
#[test]
fn intra_use_only_under_tests_config_is_test_only() {
    let alpha_default = frag(
        "alpha",
        vec![
            item(&["alpha", "test_used"], "K_T", "fn", Some("mod")),
            item(&["alpha", "prod_used"], "K_P", "fn", Some("mod")),
        ],
        vec![edge(
            &["alpha", "caller"],
            &["alpha", "prod_used"],
            "K_P",
            false,
        )],
    );
    let alpha_tests = frag(
        "alpha",
        vec![
            item(&["alpha", "test_used"], "K_T2", "fn", Some("mod")),
            item(&["alpha", "prod_used"], "K_P2", "fn", Some("mod")),
        ],
        vec![
            edge(
                &["alpha", "tests", "t"],
                &["alpha", "test_used"],
                "K_T2",
                false,
            ),
            edge(&["alpha", "caller"], &["alpha", "prod_used"], "K_P2", false),
        ],
    );
    let m = model(vec![
        ("default", vec![alpha_default]),
        ("--tests", vec![alpha_tests]),
    ]);
    let flag = |id: &str| {
        m.pub_candidates()
            .into_iter()
            .find(|c| c.id == id)
            .map(|c| (c.usage, c.test_only))
            .unwrap()
    };
    assert_eq!(flag("alpha::test_used"), (PubUsage::IntraCrate, true));
    assert_eq!(flag("alpha::prod_used"), (PubUsage::IntraCrate, false));
}

/// A feature-unified plain rlib under `--tests` carries a `DefPathHash`
/// generation NO config extracts (a feature-gated integration test's harness
/// links it — the LeaveDates `ChuckNorrisJokeEndpoint::new` E0624): the hash
/// and identity joins both miss, and the display-path fallback is what
/// resolves the edge.
#[test]
fn unextracted_hash_generation_resolves_by_display_path() {
    let alpha = frag(
        "alpha",
        vec![item(&["alpha", "helper"], "K_HLP", "fn", Some("mod"))],
        vec![],
    );
    // The integration-test crate's edge carries a third-generation hash that
    // matches no extracted def; its display path is definition-rooted.
    let it_crate = frag_target(
        "it_case",
        "test",
        vec![],
        vec![edge(
            &["it_case", "t"],
            &["alpha", "helper"],
            "K_UNEXTRACTED_GEN",
            false,
        )],
    );
    let m = model(vec![("default", vec![alpha, it_crate])]);
    let usage = m
        .pub_candidates()
        .into_iter()
        .find(|c| c.id == "alpha::helper")
        .map(|c| c.usage)
        .unwrap();
    assert_eq!(
        usage,
        PubUsage::CrossCrate,
        "the display-path fallback must credit the use"
    );
}

/// The same unextracted-generation edge can render at a RE-EXPORT path
/// (`alpha::Fraction::new` for a def at `alpha::fraction::Fraction::new`) —
/// exact path equality misses, and the suffix-relaxed leg joins it (only on
/// an unambiguous single match).
#[test]
fn unextracted_generation_reexport_path_resolves_by_suffix() {
    let alpha = frag(
        "alpha",
        vec![item(
            &["alpha", "fraction", "Fraction", "new"],
            "K_NEW",
            "fn",
            Some("impl"),
        )],
        vec![],
    );
    let user = frag(
        "beta",
        vec![],
        vec![edge(
            &["beta", "tests", "t"],
            &["alpha", "Fraction", "new"], // visible-parent re-export rendering
            "K_UNEXTRACTED_GEN2",
            false,
        )],
    );
    let m = model(vec![("default", vec![alpha, user])]);
    let usage = m
        .pub_candidates()
        .into_iter()
        .find(|c| c.id == "alpha::fraction::Fraction::new")
        .map(|c| c.usage)
        .unwrap();
    assert_eq!(usage, PubUsage::CrossCrate);
}

/// Second-order dangling (LeaveDates 2026-07-05): the import's target
/// survives, but its only real use-site in the importing crate is being
/// removed — keeping the `use` is an `unused_imports` warning. Flagged only
/// when a removed def was a user; an unrelated surviving user keeps it.
#[test]
fn dangling_imports_second_order_last_user_removed() {
    let lib = frag(
        "lib",
        vec![item(&["lib", "Theme"], "K_THEME", "struct", Some("mod"))],
        vec![],
    );
    let app = frag(
        "app",
        vec![
            item(&["app", "user"], "K_USR", "fn", Some("mod")),
            item(&["app", "keeper"], "K_KPR", "fn", Some("mod")),
        ],
        vec![
            import_edge(&["app"], &["lib", "Theme"], "K_THEME", true, 100),
            edge(&["app", "user"], &["lib", "Theme"], "K_THEME", false),
        ],
    );
    let m = model(vec![("default", vec![lib.clone(), app.clone()])]);
    let dangling = m.dangling_imports(&RemovalSet::new(["app::user"]));
    assert_eq!(
        dangling.len(),
        1,
        "Theme survives but its last `app` user is removed — the use dangles"
    );
    assert_eq!(dangling[0].elem.lo, 100);

    // A surviving user keeps the import alive…
    let mut app_kept = app.clone();
    app_kept.references.push(edge(
        &["app", "keeper"],
        &["lib", "Theme"],
        "K_THEME",
        false,
    ));
    let m = model(vec![("default", vec![lib.clone(), app_kept])]);
    assert!(
        m.dangling_imports(&RemovalSet::new(["app::user"]))
            .is_empty(),
        "keeper still references Theme — import stays"
    );

    // …and an import with NO removed user is the author's, not ours: removing
    // an unrelated def must not flag it (its users may live in cfg universes
    // the engine never extracts).
    let app_unrelated = frag(
        "app",
        vec![
            item(&["app", "user"], "K_USR", "fn", Some("mod")),
            item(&["app", "keeper"], "K_KPR", "fn", Some("mod")),
        ],
        // The import is pre-dangling: no real edge to Theme at all.
        vec![import_edge(
            &["app"],
            &["lib", "Theme"],
            "K_THEME",
            true,
            100,
        )],
    );
    let m = model(vec![("default", vec![lib, app_unrelated])]);
    assert!(
        m.dangling_imports(&RemovalSet::new(["app::keeper"]))
            .is_empty(),
        "pre-existing unused import is out of scope"
    );
}

/// Second-order dangling through trait-method prefixes: a trait import kept
/// alive only by method calls (which land on the trait *members*, never the
/// trait) dangles exactly when those calling defs are removed.
#[test]
fn dangling_imports_second_order_trait_method_users() {
    let lib = frag(
        "lib",
        vec![
            item(&["lib", "StrExt"], "K_TR", "trait", Some("mod")),
            item(&["lib", "StrExt", "shout"], "K_M", "fn", Some("trait")),
        ],
        vec![],
    );
    let app = frag(
        "app",
        vec![item(&["app", "caller"], "K_CLR", "fn", Some("mod"))],
        vec![
            import_edge(&["app"], &["lib", "StrExt"], "K_TR", true, 300),
            edge(
                &["app", "caller"],
                &["lib", "StrExt", "shout"],
                "K_M",
                false,
            ),
        ],
    );
    let m = model(vec![("default", vec![lib.clone(), app.clone()])]);
    assert_eq!(
        m.dangling_imports(&RemovalSet::new(["app::caller"])).len(),
        1,
        "the only method-call user is removed — the trait import dangles"
    );
    assert!(
        m.dangling_imports(&RemovalSet::new(["app::other"]))
            .is_empty(),
        "caller survives — the member call keeps the trait import (prefix credit)"
    );
}

/// Second-order dangling through inherent-impl members: `def_path_str` renders
/// a remote impl at the *impl's* module, so a `Type::method` call never shares
/// the imported type's path prefix — the member's `self_type` key is the only
/// linkage back to the import (the missed-workspace-trim class from the
/// 2026-07-05 LeaveDates validation).
#[test]
fn dangling_imports_second_order_inherent_method_via_self_type() {
    let mut member = item(
        // The impl lives in `lib::styles`, not under `lib::Widget` — the
        // rendering that defeats a pure path-prefix probe.
        &["lib", "styles", "<impl lib::Widget>", "paint"],
        "K_M",
        "fn",
        Some("impl"),
    );
    member.self_type = Some("K_TY".into());
    let lib = frag(
        "lib",
        vec![
            item(&["lib", "Widget"], "K_TY", "struct", Some("mod")),
            member,
        ],
        vec![],
    );
    let app = frag(
        "app",
        vec![item(&["app", "user"], "K_USR", "fn", Some("mod"))],
        vec![
            import_edge(&["app"], &["lib", "Widget"], "K_TY", false, 100),
            edge(
                &["app", "user"],
                &["lib", "styles", "<impl lib::Widget>", "paint"],
                "K_M",
                false,
            ),
        ],
    );
    let m = model(vec![("default", vec![lib.clone(), app.clone()])]);
    assert_eq!(
        m.dangling_imports(&RemovalSet::new(["app::user"])).len(),
        1,
        "the only `Widget::paint` caller is removed — the type import dangles"
    );
    assert!(
        m.dangling_imports(&RemovalSet::new(["app::other"]))
            .is_empty(),
        "caller survives — the member call credits the self type"
    );
}

/// Second-order dangling of an OUT-OF-WORKSPACE import (`use anyhow::Context`):
/// the target resolves to no def and no identity, so it is tracked under its
/// display-path pseudo-identity — dangling exactly when every real reference
/// under that path came from a removed def, kept by any survivor, and never
/// flagged without a removed user (causality gate).
#[test]
fn dangling_imports_second_order_external_target() {
    let app = frag(
        "app",
        vec![
            item(&["app", "user"], "K_USR", "fn", Some("mod")),
            item(&["app", "keeper"], "K_KPR", "fn", Some("mod")),
        ],
        vec![
            import_edge(&["app"], &["anyhow", "Context"], "K_EXT_TR", false, 200),
            // A method call lands on the trait member — the prefix probe
            // covers it, exactly like workspace trait imports.
            edge(
                &["app", "user"],
                &["anyhow", "Context", "context"],
                "K_EXT_M",
                false,
            ),
        ],
    );
    let m = model(vec![("default", vec![app.clone()])]);
    assert_eq!(
        m.dangling_imports(&RemovalSet::new(["app::user"])).len(),
        1,
        "the only `.context()` caller is removed — the external import dangles"
    );

    let mut app_kept = app.clone();
    app_kept.references.push(edge(
        &["app", "keeper"],
        &["anyhow", "Context", "context"],
        "K_EXT_M",
        false,
    ));
    let m = model(vec![("default", vec![app_kept])]);
    assert!(
        m.dangling_imports(&RemovalSet::new(["app::user"]))
            .is_empty(),
        "keeper still calls `.context()` — the import stays"
    );

    // Pre-existing unused external import + unrelated removal: not ours.
    let app_pre = frag(
        "app",
        vec![item(&["app", "keeper"], "K_KPR", "fn", Some("mod"))],
        vec![import_edge(
            &["app"],
            &["anyhow", "Context"],
            "K_EXT_TR",
            false,
            200,
        )],
    );
    let m = model(vec![("default", vec![app_pre])]);
    assert!(
        m.dangling_imports(&RemovalSet::new(["app::keeper"]))
            .is_empty(),
        "no removed user — the pre-existing unused import is the author's"
    );
}

/// A same-crate `use super::*` glob for the fixtures below: references in
/// `from`'s module may resolve through imports in module `to`.
fn glob_edge(from: &[&str], to: &[&str]) -> RefEdge {
    let mut e = edge(from, to, "K_NOJOIN", true);
    e.glob = true;
    e.reexport = false;
    e
}

/// The second-order check is scoped to the importing **module**: rustc judges
/// each `use` statement by its own module, so a nested `#[cfg(test)] mod`
/// with its *own* explicit import of the name resolves there — its uses
/// cannot keep the outer import alive (the `parse_f32` / `TimeView` residue
/// classes from the 2026-07-06 delete-mode re-validation).
#[test]
fn dangling_imports_second_order_scoped_to_importing_module() {
    let lib = frag(
        "lib",
        vec![item(&["lib", "Theme"], "K_THEME", "struct", Some("mod"))],
        vec![],
    );
    // Outer import + removed user at the crate root; a nested `tests` module
    // with its own import and a surviving user.
    let app = frag(
        "app",
        vec![
            item(&["app", "user"], "K_USR", "fn", Some("mod")),
            item(&["app", "tests"], "K_MOD", "mod", Some("mod")),
            item(&["app", "tests", "t"], "K_T", "fn", Some("mod")),
        ],
        vec![
            import_edge(&["app"], &["lib", "Theme"], "K_THEME", true, 100),
            import_edge(&["app", "tests"], &["lib", "Theme"], "K_THEME", true, 500),
            edge(&["app", "user"], &["lib", "Theme"], "K_THEME", false),
            edge(&["app", "tests", "t"], &["lib", "Theme"], "K_THEME", false),
        ],
    );
    let m = model(vec![("default", vec![lib.clone(), app.clone()])]);
    let dangling = m.dangling_imports(&RemovalSet::new(["app::user"]));
    assert_eq!(
        dangling.len(),
        1,
        "the test module resolves Theme through its own import — the outer one dangles"
    );
    assert_eq!(
        dangling[0].elem.lo, 100,
        "the OUTER leaf is the dangling one"
    );

    // Remove the test fn instead: the nested module's own import dangles,
    // the outer import is kept by its surviving root user.
    let dangling = m.dangling_imports(&RemovalSet::new(["app::tests::t"]));
    assert_eq!(dangling.len(), 1);
    assert_eq!(
        dangling[0].elem.lo, 500,
        "the NESTED leaf is the dangling one"
    );
}

/// `use super::*` bridges module scopes: a test module *without* its own
/// import resolves the name through the parent's import, so its surviving
/// uses must keep that import (deleting it would break `super::*`
/// resolution) — and its removed uses must dangle it. An explicit own import
/// beats the glob (rustc prefers the explicit binding), flipping both.
#[test]
fn dangling_imports_glob_reimport_bridges_module_scopes() {
    let lib = frag(
        "lib",
        vec![item(&["lib", "Theme"], "K_THEME", "struct", Some("mod"))],
        vec![],
    );
    let app = frag(
        "app",
        vec![
            item(&["app", "user"], "K_USR", "fn", Some("mod")),
            item(&["app", "tests"], "K_MOD", "mod", Some("mod")),
            item(&["app", "tests", "t"], "K_T", "fn", Some("mod")),
        ],
        vec![
            import_edge(&["app"], &["lib", "Theme"], "K_THEME", true, 100),
            glob_edge(&["app", "tests"], &["app"]),
            edge(&["app", "user"], &["lib", "Theme"], "K_THEME", false),
            edge(&["app", "tests", "t"], &["lib", "Theme"], "K_THEME", false),
        ],
    );
    let m = model(vec![("default", vec![lib.clone(), app.clone()])]);
    assert!(
        m.dangling_imports(&RemovalSet::new(["app::user"]))
            .is_empty(),
        "the test fn reaches the outer import via `use super::*` — it stays"
    );
    assert_eq!(
        m.dangling_imports(&RemovalSet::new(["app::user", "app::tests::t"]))
            .len(),
        1,
        "both users (one via the glob) removed — the outer import dangles"
    );

    // Adding an explicit import in the test module re-routes its resolution:
    // the glob no longer reaches the outer import, which dangles again.
    let mut app_own = app.clone();
    app_own.references.push(import_edge(
        &["app", "tests"],
        &["lib", "Theme"],
        "K_THEME",
        true,
        500,
    ));
    let m = model(vec![("default", vec![lib, app_own])]);
    let dangling = m.dangling_imports(&RemovalSet::new(["app::user"]));
    assert_eq!(
        dangling.len(),
        1,
        "explicit beats glob — the test module's uses no longer shield the outer import"
    );
    assert_eq!(dangling[0].elem.lo, 100);
}

/// Span-less `in_signature` edges are lowered-type projections, not source
/// name-resolutions: `static N: GlobalSignal<T>` normalizes through `Signal`
/// without the source ever writing it, and rustc's `unused_imports` never
/// sees such a "use" (the `Signal` residue class from the 2026-07-06
/// delete-mode re-validation). They must not shield an import.
#[test]
fn dangling_imports_ignore_lowered_signature_edges() {
    let lib = frag(
        "lib",
        vec![item(&["lib", "Signal"], "K_SIG", "struct", Some("mod"))],
        vec![],
    );
    let mut sig_edge = edge(&["app", "NOTICES"], &["lib", "Signal"], "K_SIG", false);
    sig_edge.in_signature = true;
    let app = frag(
        "app",
        vec![
            item(&["app", "user"], "K_USR", "fn", Some("mod")),
            item(&["app", "NOTICES"], "K_NOT", "static", Some("mod")),
        ],
        vec![
            import_edge(&["app"], &["lib", "Signal"], "K_SIG", true, 100),
            edge(&["app", "user"], &["lib", "Signal"], "K_SIG", false),
            sig_edge,
        ],
    );
    let m = model(vec![("default", vec![lib, app])]);
    assert_eq!(
        m.dangling_imports(&RemovalSet::new(["app::user"])).len(),
        1,
        "the surviving lowered-signature edge is not a source use — the import dangles"
    );
}

/// Receiver-based resolutions bypass imports: an inherent `.time()` call
/// resolves from the receiver's type, never through `use …::TimeView;` — so
/// a surviving method call must not shield the import once its written users
/// are removed (the `TimeView` residue class from the 2026-07-06 delete-mode
/// re-validation). A *written* `TimeView::assoc` path does shield; a *trait*
/// method call shields its trait's import (the trait must be in scope).
#[test]
fn dangling_imports_inherent_method_calls_do_not_shield() {
    // TimeView + an inherent method in the type's own module (renders under
    // the type, so the prefix probe would match it without the flag).
    let lib = frag(
        "lib",
        vec![
            item(&["lib", "TimeView"], "K_TV", "struct", Some("mod")),
            item(&["lib", "TimeView", "time"], "K_TM", "fn", Some("impl")),
        ],
        vec![],
    );
    let mut call = edge(
        &["app", "keeper"],
        &["lib", "TimeView", "time"],
        "K_TM",
        false,
    );
    call.receiver_resolved = true;
    let app = frag(
        "app",
        vec![
            item(&["app", "user"], "K_USR", "fn", Some("mod")),
            item(&["app", "keeper"], "K_KPR", "fn", Some("mod")),
        ],
        vec![
            import_edge(&["app"], &["lib", "TimeView"], "K_TV", true, 100),
            // the written user (struct literal / `TimeView::assoc` path)
            edge(&["app", "user"], &["lib", "TimeView"], "K_TV", false),
            // the surviving `.time()` call
            call.clone(),
        ],
    );
    let m = model(vec![("default", vec![lib.clone(), app.clone()])]);
    assert_eq!(
        m.dangling_imports(&RemovalSet::new(["app::user"])).len(),
        1,
        "keeper's `.time()` never resolved through the import — it dangles"
    );

    // The same edge as a WRITTEN `TimeView::time` path keeps the import.
    let mut app_written = app.clone();
    for e in &mut app_written.references {
        e.receiver_resolved = false;
    }
    let m = model(vec![("default", vec![lib, app_written])]);
    assert!(
        m.dangling_imports(&RemovalSet::new(["app::user"]))
            .is_empty(),
        "a written `TimeView::time` path resolves through the import — it stays"
    );
}

/// The trait-member exception to the receiver-resolution rule: `.shout()`
/// only resolves with `use lib::StrExt;` in scope, so a surviving trait
/// method call must keep the trait import even though it is receiver-based.
#[test]
fn dangling_imports_trait_method_calls_do_shield() {
    let lib = frag(
        "lib",
        vec![
            item(&["lib", "StrExt"], "K_TR", "trait", Some("mod")),
            item(&["lib", "StrExt", "shout"], "K_M", "fn", Some("trait")),
        ],
        vec![],
    );
    let mut call = edge(
        &["app", "keeper"],
        &["lib", "StrExt", "shout"],
        "K_M",
        false,
    );
    call.receiver_resolved = true;
    let app = frag(
        "app",
        vec![
            item(&["app", "user"], "K_USR", "fn", Some("mod")),
            item(&["app", "keeper"], "K_KPR", "fn", Some("mod")),
        ],
        vec![
            import_edge(&["app"], &["lib", "StrExt"], "K_TR", true, 300),
            edge(&["app", "user"], &["lib", "StrExt"], "K_TR", false),
            call,
        ],
    );
    let m = model(vec![("default", vec![lib, app])]);
    assert!(
        m.dangling_imports(&RemovalSet::new(["app::user"]))
            .is_empty(),
        "keeper's `.shout()` needs StrExt in scope — the trait import stays"
    );
}

/// `edge` as the v6 extractor emits a method call: `receiver_resolved` plus
/// the lexical [`RefEdge::from_module`].
fn recv_edge_in_module(from: &[&str], module: &[&str], to: &[&str], to_key: &str) -> RefEdge {
    let mut e = edge(from, to, to_key, false);
    e.receiver_resolved = true;
    e.from_module = module.iter().map(|s| s.to_string()).collect();
    e
}

/// A trait-impl body's references credit the imports of the module the impl
/// is lexically in (the LeaveDates `DateFn`/`from_iter` breakage, 2026-07-06):
/// `def_path_str` renders the impl as `<list::List as FromIterator<…>>` — the
/// real module appears only inside the bracket, so the textual prefix walk
/// never reaches scope `app::list` and a surviving trait-method call there
/// credited no module at all. The edge's lexical `from_module` is the fix.
#[test]
fn dangling_imports_trait_impl_body_credits_lexical_module() {
    let lib = frag(
        "app",
        vec![
            item(&["app", "datefn", "DateFn"], "K_TR", "trait", Some("mod")),
            // Provided method (default body in the trait): a call resolves to
            // the trait's own def, so `trait_parent` is the only linkage.
            item(
                &["app", "datefn", "DateFn", "year"],
                "K_M",
                "fn",
                Some("trait"),
            ),
        ],
        vec![
            import_edge(
                &["app", "list"],
                &["app", "datefn", "DateFn"],
                "K_TR",
                true,
                100,
            ),
            // The removed user: an inherent-impl fn in `app::list`.
            recv_edge_in_module(
                &["app", "list", "List", "dead_sort"],
                &["app", "list"],
                &["app", "datefn", "DateFn", "year"],
                "K_M",
            ),
            // The SURVIVING user: a trait-impl fn in the same module, whose
            // rendered from-path hides `list` inside the bracket segment.
            recv_edge_in_module(
                &[
                    "app",
                    "<list",
                    "List as std",
                    "iter",
                    "FromIterator<date",
                    "Date>>",
                    "from_iter",
                ],
                &["app", "list"],
                &["app", "datefn", "DateFn", "year"],
                "K_M",
            ),
        ],
    );
    let m = model(vec![("default", vec![lib.clone()])]);
    assert!(
        m.dangling_imports(&RemovalSet::new(["app::list::List::dead_sort"]))
            .is_empty(),
        "from_iter's `.year()` still needs DateFn in scope — the import stays"
    );

    // Remove the surviving trait-impl edge: now the deleted fn really was the
    // last user, and the import must dangle (guards against over-crediting).
    let mut last_user = lib;
    last_user.references.pop();
    let m = model(vec![("default", vec![last_user])]);
    assert_eq!(
        m.dangling_imports(&RemovalSet::new(["app::list::List::dead_sort"]))
            .len(),
        1,
        "no surviving user — the import dangles"
    );
}

// --- the clippy-unmask guard (narrowing strips `avoid-breaking-exported-api`) ---

fn method(
    path: &[&str],
    key: &str,
    self_type: Option<&str>,
    self_kind: &str,
    self_copy: Option<bool>,
) -> ItemFact {
    let mut it = item(path, key, "fn", Some("impl"));
    it.self_type = self_type.map(String::from);
    it.self_kind = Some(self_kind.into());
    it.self_copy = self_copy;
    it
}

fn unmask_of(m: &SemanticModel, id: &str) -> Option<(String, String)> {
    m.pub_candidates()
        .into_iter()
        .find(|c| c.id == id)
        .unwrap_or_else(|| panic!("no candidate {id}"))
        .narrow_unmask
        .map(|u| (u.lint.to_string(), u.member))
}

/// `wrong_self_convention`: an `is_*` method taking `self` by value on a
/// non-`Copy` type violates clippy's table — narrowing the type (or the
/// method itself) would unmask it. On a `Copy` type, by-value `self`
/// satisfies the reference slot and the guard stays quiet.
#[test]
fn narrow_unmask_replays_wrong_self_convention() {
    let mk = |copy: bool| {
        frag(
            "alpha",
            vec![
                item(&["alpha", "Widget"], "K_W", "struct", Some("mod")),
                method(
                    &["alpha", "Widget", "is_open"],
                    "K_IO",
                    Some("K_W"),
                    "value",
                    Some(copy),
                ),
            ],
            vec![],
        )
    };
    let m = model(vec![("default", vec![mk(false)])]);
    let hit = ("wrong_self_convention".to_string(), "is_open".to_string());
    assert_eq!(unmask_of(&m, "alpha::Widget").as_ref(), Some(&hit));
    assert_eq!(unmask_of(&m, "alpha::Widget::is_open").as_ref(), Some(&hit));

    let m = model(vec![("default", vec![mk(true)])]);
    assert_eq!(unmask_of(&m, "alpha::Widget"), None, "Copy satisfies is_*");

    // A conforming method never trips the guard.
    let ok = frag(
        "alpha",
        vec![
            item(&["alpha", "Widget"], "K_W", "struct", Some("mod")),
            method(
                &["alpha", "Widget", "is_open"],
                "K_IO",
                Some("K_W"),
                "ref",
                Some(false),
            ),
        ],
        vec![],
    );
    let m = model(vec![("default", vec![ok])]);
    assert_eq!(unmask_of(&m, "alpha::Widget"), None);
}

/// `len_without_is_empty`: a pub `len(&self)` with no `is_empty` sibling
/// unmasks; adding the sibling clears it.
#[test]
fn narrow_unmask_replays_len_without_is_empty() {
    let lenful = |with_is_empty: bool| {
        let mut items = vec![
            item(&["alpha", "List"], "K_L", "struct", Some("mod")),
            method(
                &["alpha", "List", "len"],
                "K_LEN",
                Some("K_L"),
                "ref",
                Some(false),
            ),
        ];
        if with_is_empty {
            items.push(method(
                &["alpha", "List", "is_empty"],
                "K_IE",
                Some("K_L"),
                "ref",
                Some(false),
            ));
        }
        frag("alpha", items, vec![])
    };
    let m = model(vec![("default", vec![lenful(false)])]);
    assert_eq!(
        unmask_of(&m, "alpha::List"),
        Some(("len_without_is_empty".to_string(), "len".to_string()))
    );
    let m = model(vec![("default", vec![lenful(true)])]);
    assert_eq!(unmask_of(&m, "alpha::List"), None);
}

/// Trait declarations guard through their members too: `to_*` on a trait item
/// expects `&self` (the `Copy`-by-value row is inherent-only).
#[test]
fn narrow_unmask_covers_trait_decl_members() {
    let mut to_px = item(&["alpha", "Px", "to_px"], "K_TP", "fn", Some("trait"));
    to_px.self_kind = Some("value".into());
    let tr = frag(
        "alpha",
        vec![item(&["alpha", "Px"], "K_T", "trait", Some("mod")), to_px],
        vec![],
    );
    let m = model(vec![("default", vec![tr])]);
    assert_eq!(
        unmask_of(&m, "alpha::Px"),
        Some(("wrong_self_convention".to_string(), "to_px".to_string()))
    );
}

/// The dead-field narrow guard (the `CacheLock.path` case from the 2026-07-06
/// re-validation): a type with a field no READ edge reaches gates its tighten
/// — rustc `dead_code` exempts a `pub` type's fields but not a `pub(crate)`
/// one's. A field read lifts the gate; underscore-prefixed fields are exempt
/// like rustc exempts them.
#[test]
fn narrowing_type_with_unread_field_is_gated() {
    let mk = |field_name: &str, with_read: bool| {
        let field_path = ["alpha", "CacheLock", field_name];
        let mut refs = vec![edge(
            &["alpha", "user"],
            &["alpha", "CacheLock"],
            "K_S",
            false,
        )];
        if with_read {
            refs.push(edge(&["alpha", "user"], &field_path, "K_F", false));
        }
        frag(
            "alpha",
            vec![
                item(&["alpha", "CacheLock"], "K_S", "struct", Some("mod")),
                item(&field_path, "K_F", "field", Some("other")),
                item(&["alpha", "user"], "K_U", "fn", Some("mod")),
            ],
            refs,
        )
    };
    let dead_fields = |m: &SemanticModel| {
        m.pub_candidates()
            .into_iter()
            .find(|c| c.id == "alpha::CacheLock")
            .unwrap()
            .dead_fields
    };
    let m = model(vec![("default", vec![mk("path", false)])]);
    assert!(dead_fields(&m), "write-only field gates the narrow");
    let m = model(vec![("default", vec![mk("path", true)])]);
    assert!(!dead_fields(&m), "a read lifts the gate");
    let m = model(vec![("default", vec![mk("_file", false)])]);
    assert!(!dead_fields(&m), "underscore fields are dead_code-exempt");
}

// --- private collateral: the second-order dead code a cascade strands ---

fn private_item(path: &[&str], key: &str, kind: &str, parent: Option<&str>) -> ItemFact {
    let mut it = item(path, key, kind, parent);
    it.visibility = Visibility::Restricted("crate".into());
    it
}

/// A private helper whose only caller is removed is a collateral orphan —
/// but a surviving caller, or no prior caller at all (causality: pre-existing
/// dead code is the author's, likely `#[allow]`ed or cfg-shifted), keeps it out.
#[test]
fn private_orphans_causality_gated() {
    let app = frag(
        "app",
        vec![
            item(&["app", "user"], "K_U", "fn", Some("mod")),
            item(&["app", "keeper"], "K_K", "fn", Some("mod")),
            private_item(&["app", "helper"], "K_H", "fn", Some("mod")),
        ],
        vec![edge(&["app", "user"], &["app", "helper"], "K_H", false)],
    );
    let m = model(vec![("default", vec![app.clone()])]);
    let orphans = m.private_orphans(&RemovalSet::new(["app::user"]));
    assert_eq!(
        orphans.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(),
        ["app::helper"],
        "last caller removed — the private helper is stranded"
    );

    let mut app_kept = app.clone();
    app_kept
        .references
        .push(edge(&["app", "keeper"], &["app", "helper"], "K_H", false));
    let m = model(vec![("default", vec![app_kept])]);
    assert!(
        m.private_orphans(&RemovalSet::new(["app::user"]))
            .is_empty(),
        "a surviving caller keeps the helper"
    );

    // Pre-existing dead private code: no edge before the removal either.
    let app_pre = frag(
        "app",
        vec![
            item(&["app", "user"], "K_U", "fn", Some("mod")),
            private_item(&["app", "helper"], "K_H", "fn", Some("mod")),
        ],
        vec![],
    );
    let m = model(vec![("default", vec![app_pre])]);
    assert!(
        m.private_orphans(&RemovalSet::new(["app::user"]))
            .is_empty(),
        "never used before the removal — not our collateral"
    );
}

/// The guards: ADTs stay (deleting one orphans its `impl` blocks), pub defs
/// are the cascade's own domain, and a use-site in ANY config keeps the def.
#[test]
fn private_orphans_guards_and_config_union() {
    let app = frag(
        "app",
        vec![
            item(&["app", "user"], "K_U", "fn", Some("mod")),
            private_item(&["app", "Cfg"], "K_S", "struct", Some("mod")),
            private_item(&["app", "helper"], "K_H", "fn", Some("mod")),
        ],
        vec![
            edge(&["app", "user"], &["app", "Cfg"], "K_S", false),
            edge(&["app", "user"], &["app", "helper"], "K_H", false),
        ],
    );
    // Under --tests, a surviving test fn also calls the helper.
    let mut app_tests = app.clone();
    app_tests.references.push(edge(
        &["app", "tests", "check"],
        &["app", "helper"],
        "K_H",
        false,
    ));
    let m = model(vec![
        ("default", vec![app.clone()]),
        ("tests", vec![app_tests]),
    ]);
    assert!(
        m.private_orphans(&RemovalSet::new(["app::user"]))
            .is_empty(),
        "struct is kind-guarded; helper stays alive via the tests config"
    );

    // Without the tests-config caller, only the fn falls out — never the ADT.
    let m = model(vec![("default", vec![app])]);
    let orphans = m.private_orphans(&RemovalSet::new(["app::user"]));
    assert_eq!(
        orphans.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(),
        ["app::helper"]
    );
}

/// A stranded helper's own callees strand transitively once the helper joins
/// the removal set — the engine half of the cascade loop.
#[test]
fn private_orphans_chain_through_removed_helpers() {
    let app = frag(
        "app",
        vec![
            item(&["app", "user"], "K_U", "fn", Some("mod")),
            private_item(&["app", "helper"], "K_H", "fn", Some("mod")),
            private_item(&["app", "inner"], "K_I", "fn", Some("mod")),
        ],
        vec![
            edge(&["app", "user"], &["app", "helper"], "K_H", false),
            edge(&["app", "helper"], &["app", "inner"], "K_I", false),
        ],
    );
    let m = model(vec![("default", vec![app])]);
    let first = m.private_orphans(&RemovalSet::new(["app::user"]));
    assert_eq!(
        first.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(),
        ["app::helper"],
        "inner is still held by helper in round one"
    );
    let second = m.private_orphans(&RemovalSet::new(["app::user", "app::helper"]));
    assert_eq!(
        second.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(),
        ["app::helper", "app::inner"],
        "with helper removed, inner strands too"
    );
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
