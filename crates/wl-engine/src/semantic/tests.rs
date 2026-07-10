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
        glob_used_names: Vec::new(),
        trait_scope: false,
        extern_root: false,
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
        is_test_cfg: false,
        items,
        references,
        loaded_files: Vec::new(),
    }
}

/// The `+test` cfg variant of a lib: same `target_kind`, `is_test_cfg` set —
/// exactly what the extractor emits for a unit compiled with `--test`.
fn frag_test_cfg(name: &str, items: Vec<ItemFact>, references: Vec<RefEdge>) -> IrFragment {
    IrFragment {
        is_test_cfg: true,
        ..frag(name, items, references)
    }
}

/// A fragment that carries nothing but the files rustc opened — the substrate
/// `orphan-file` judges. Paths go in absolute (as the extractor emits them);
/// the assembler is what normalizes them workspace-relative.
fn frag_files(name: &str, loaded_files: &[&str]) -> IrFragment {
    IrFragment {
        loaded_files: loaded_files.iter().map(|f| (*f).to_string()).collect(),
        ..frag(name, vec![], vec![])
    }
}

fn model(configs: Vec<(&str, Vec<IrFragment>)>) -> SemanticModel {
    // Route native fixtures through the same archived core production uses.
    let configs = configs
        .into_iter()
        .map(|(id, frags)| {
            (
                id.to_string(),
                frags.iter().map(FragmentBytes::owned).collect(),
            )
        })
        .collect();
    SemanticModel::assemble_bytes(configs, fixture_meta()).unwrap()
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
        .map(|n| (n.name.as_str(), n.reason))
        .collect();
    assert!(not_judged.contains(&("dev_helper", NotJudged::DevWithoutTestConfig)));
    assert!(not_judged.contains(&("hook_installer", NotJudged::BuildDep)));
    assert!(not_judged.contains(&("feature_gated", NotJudged::Optional)));
    // Platform-gated: never judged, even though it is a plain normal dep.
    assert!(not_judged.contains(&("platform_only", NotJudged::TargetGated)));

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

/// Unused-deps: a member no config compiled (zero fragments) has unjudgeable
/// deps — the verdict routes its would-be-judged normal deps to
/// [`NotJudged::NotCompiled`], never to `unused`, and `DepUsage::crate_compiled`
/// reports the gap. (`deps_verdict_scopes_and_facades` is the compiled-member
/// counterpart: there the same `never_used` dep IS flagged.) Dev/build/optional
/// deps keep their own, more-specific exempt reasons.
#[test]
fn deps_verdict_uncompiled_member_routes_to_not_compiled() {
    // Only `beta` compiled; `alpha` (which declares deps) produced no fragment.
    let m = model(vec![("default", vec![frag("beta", vec![], vec![])])]);

    let usage = m.dep_usage();
    assert!(usage.crate_compiled("beta"), "beta has a fragment");
    assert!(
        !usage.crate_compiled("alpha"),
        "alpha produced no fragment — its deps are unjudgeable"
    );

    let v = m.deps_verdict();
    let alpha = v.crates.iter().find(|c| c.krate == "alpha").unwrap();
    assert!(
        alpha.unused.is_empty(),
        "an uncompiled member's deps are never flagged as unused"
    );
    let not_judged: Vec<(&str, NotJudged)> = alpha
        .not_judged
        .iter()
        .map(|n| (n.name.as_str(), n.reason))
        .collect();
    // Normal deps that would be judged if compiled land under NotCompiled.
    assert!(not_judged.contains(&("facade", NotJudged::NotCompiled)));
    assert!(not_judged.contains(&("never_used", NotJudged::NotCompiled)));
    assert!(not_judged.contains(&("md_5", NotJudged::NotCompiled)));
    // Dev/build/optional/target-gated deps keep their more-specific reasons.
    assert!(not_judged.contains(&("dev_helper", NotJudged::DevWithoutTestConfig)));
    assert!(not_judged.contains(&("hook_installer", NotJudged::BuildDep)));
    assert!(not_judged.contains(&("feature_gated", NotJudged::Optional)));
    assert!(not_judged.contains(&("platform_only", NotJudged::TargetGated)));
}

/// Unused-deps: a re-export shim dep is credited through [`RefEdge::via`] —
/// the resolved target defines the item in `std` (a sysroot crate outside
/// every cargo closure), and only the written path root names the dep. An
/// edge with `via == None` (the plain, non-shim shape) must keep the dep
/// flagged.
#[test]
fn deps_verdict_credits_reexport_shim_via_written_root() {
    // A plain edge (via == None): the std target credits nothing.
    let old_shape = edge(
        &["alpha", "user"],
        &["std", "time", "Duration"],
        "K_STD",
        true,
    );
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
        is_test_cfg: false,
        items: Vec::new(),
        references,
        loaded_files: Vec::new(),
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

    // A `.wlir` written under a different schema version: a valid archive with
    // a stale header. The header gate rejects it before any archived access,
    // exactly as `check_schema` used to on the JSON path.
    let mut bytes = wl_ir::write_bytes(&frag("alpha", vec![], vec![])).unwrap();
    bytes[4] = bytes[4].wrapping_add(1); // corrupt SCHEMA_VERSION in the header
    std::fs::write(tmp.path().join("alpha.wlir"), &bytes).unwrap();
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
    let dangling = m.dangling_imports(
        &RemovalSet::new(["app::inner::helper"]),
        &Default::default(),
    );
    assert_eq!(dangling.len(), 1, "only the helper import dangles");
    let d = &dangling[0];
    assert_eq!(d.elem.lo, 100, "the helper leaf, not the kept one");
    assert!(
        d.decl.lo == d.elem.lo && d.decl.hi == d.elem.hi,
        "brace-leaf: decl == elem (the excise discriminator)"
    );
    assert!(!d.reexport);
}

/// A `use` declared inside a generated (`include!`d) file is surgery's no-go
/// zone: its target is excision-blocked (the cascade must never delete an item
/// whose import cleanup would mean editing a file the generator owns), and the
/// import itself never dangles. Without the generated set both revert to the
/// plain behavior — the same edges are blockable and excisable.
#[test]
fn generated_file_imports_block_excision_and_never_dangle() {
    let mut import = import_edge(&["app"], &["app", "inner", "helper"], "K_HLP", true, 100);
    // Relocate the decl into the generated file; `include!` splices it into
    // `app`'s module scope, so `from` stays the including module.
    for s in [&mut import.decl_span, &mut import.elem_span] {
        s.as_mut().unwrap().file = "crates/app/src/gen.rs".into();
    }
    let app = frag(
        "app",
        vec![item(
            &["app", "inner", "helper"],
            "K_HLP",
            "fn",
            Some("mod"),
        )],
        vec![import],
    );
    let m = model(vec![("default", vec![app])]);
    let generated: std::collections::HashSet<std::path::PathBuf> =
        [std::path::PathBuf::from("crates/app/src/gen.rs")].into();

    assert_eq!(
        m.import_excision_blocked(&generated)
            .get("app::inner::helper"),
        Some(&ExcisionBlock::GeneratedFile)
    );
    assert!(
        m.dangling_imports(&RemovalSet::new(["app::inner::helper"]), &generated)
            .is_empty(),
        "surgery must not touch a generated file"
    );

    assert!(m.import_excision_blocked(&Default::default()).is_empty());
    assert_eq!(
        m.dangling_imports(
            &RemovalSet::new(["app::inner::helper"]),
            &Default::default()
        )
        .len(),
        1,
        "same edges, no generated set: the plain first-order dangle"
    );
}

/// When one target is named both by a macro-generated `use` and by one in a
/// generated file, the macro-generated reason wins (the stricter claim — no
/// edit surface exists anywhere), regardless of edge order.
#[test]
fn excision_block_macro_reason_beats_generated_file() {
    let mut in_generated = import_edge(&["app"], &["app", "helper"], "K_HLP", true, 100);
    for s in [&mut in_generated.decl_span, &mut in_generated.elem_span] {
        s.as_mut().unwrap().file = "crates/app/src/gen.rs".into();
    }
    let mut from_macro = import_edge(&["app"], &["app", "helper"], "K_HLP", true, 200);
    from_macro.decl_span.as_mut().unwrap().from_expansion = true;
    let app = frag(
        "app",
        vec![item(&["app", "helper"], "K_HLP", "fn", Some("mod"))],
        // Generated-file edge first, so the merge (not insertion order) must
        // produce the macro reason.
        vec![in_generated, from_macro],
    );
    let m = model(vec![("default", vec![app])]);
    let generated: std::collections::HashSet<std::path::PathBuf> =
        [std::path::PathBuf::from("crates/app/src/gen.rs")].into();
    assert_eq!(
        m.import_excision_blocked(&generated).get("app::helper"),
        Some(&ExcisionBlock::MacroGenerated)
    );
}

/// An item whose every use-site is test-cfg-gated (`IntraCrate` reached only
/// outside the primary config) is flagged `test_only`: narrowing it compiles
/// but leaves it `dead_code` on the plain build, so `--fix` must not apply.
#[test]
fn intra_use_only_in_test_cfg_variant_is_test_only() {
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
    // The `--tests` config compiles the crate's `+test` cfg variant: cfg(test)
    // code AND the plain caller both land in a fragment carrying is_test_cfg —
    // it is the plain config's fragment that proves production reach.
    let alpha_tests = frag_test_cfg(
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
    let usage = |id: &str| {
        m.pub_candidates()
            .into_iter()
            .find(|c| c.id == id)
            .map(|c| c.usage)
            .unwrap()
    };
    assert_eq!(usage("alpha::test_used"), PubUsage::TestOnly);
    assert_eq!(usage("alpha::prod_used"), PubUsage::IntraCrate);
}

/// Intra reach only under a non-home config's PLAIN units — a caller gated
/// behind a cfg the home build never compiles (`--target`-only, feature-gated).
/// Production-used, so `IntraCrate`, but `pub(crate)` would trip `dead_code`
/// on the home build: the off-home gate must hold so the narrow is shown, not
/// machine-applied. (The test/bench flavor of this used to share the flag; it
/// is now the provenance-judged `TestOnly` verdict above.)
#[test]
fn intra_use_only_off_home_config_gates_narrow() {
    let alpha_default = frag(
        "alpha",
        vec![item(&["alpha", "wasm_only"], "K_W", "fn", Some("mod"))],
        vec![],
    );
    let alpha_wasm = frag(
        "alpha",
        vec![item(&["alpha", "wasm_only"], "K_W2", "fn", Some("mod"))],
        vec![edge(
            &["alpha", "wasm_caller"],
            &["alpha", "wasm_only"],
            "K_W2",
            false,
        )],
    );
    let m = model(vec![
        ("default", vec![alpha_default]),
        ("--target wasm32", vec![alpha_wasm]),
    ]);
    let cand = m
        .pub_candidates()
        .into_iter()
        .find(|c| c.id == "alpha::wasm_only")
        .unwrap();
    assert_eq!(cand.usage, PubUsage::IntraCrate);
    assert!(cand.intra_off_home, "off-home reach must gate the narrow");
}

/// The mixed shape: production use-sites inside the owning crate PLUS
/// cross-crate reach from another member's test code. Not dead — and not
/// `IntraCrate` either: tightening to `pub(crate)` would break the
/// referencing test (it compiles as another crate). Leave alone.
#[test]
fn prod_intra_plus_cross_test_reach_leaves_alone() {
    let alpha = frag(
        "alpha",
        vec![item(&["alpha", "mixed"], "K_M", "fn", Some("mod"))],
        vec![edge(
            &["alpha", "caller"],
            &["alpha", "mixed"],
            "K_M",
            false,
        )],
    );
    let beta_tests = frag_test_cfg(
        "beta",
        vec![],
        vec![edge_ext(
            &["beta", "tests", "t"],
            &["alpha", "mixed"],
            "K_M",
            false,
            true,
        )],
    );
    let m = model(vec![("default", vec![alpha, beta_tests])]);
    let usage = m
        .pub_candidates()
        .into_iter()
        .find(|c| c.id == "alpha::mixed")
        .map(|c| c.usage)
        .unwrap();
    assert_eq!(
        usage,
        PubUsage::CrossCrate,
        "prod-intra + cross-test reach must neither tighten nor report dead"
    );
}

/// Cross-crate test reach alone — another member's `#[cfg(test)]` module is
/// the ONLY referrer (the `render_one` shape, single-config edition): the
/// dead-family TestOnly verdict, where the old classifier read CrossCrate and
/// stayed silent.
#[test]
fn cross_crate_test_only_reach_is_test_only() {
    let alpha = frag(
        "alpha",
        vec![item(&["alpha", "embalmed"], "K_E", "fn", Some("mod"))],
        vec![],
    );
    let beta_tests = frag_test_cfg(
        "beta",
        vec![],
        vec![edge_ext(
            &["beta", "tests", "t"],
            &["alpha", "embalmed"],
            "K_E",
            false,
            true,
        )],
    );
    let m = model(vec![("default", vec![alpha, beta_tests])]);
    let usage = m
        .pub_candidates()
        .into_iter()
        .find(|c| c.id == "alpha::embalmed")
        .map(|c| c.usage)
        .unwrap();
    assert_eq!(usage, PubUsage::TestOnly);
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
    // TestOnly, not Unused: the fallback credited the edge (and, the referrer
    // being an integration-test crate, provenance classifies it test reach).
    assert_eq!(
        usage,
        PubUsage::TestOnly,
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
    let dangling = m.dangling_imports(&RemovalSet::new(["app::user"]), &Default::default());
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
        m.dangling_imports(&RemovalSet::new(["app::user"]), &Default::default())
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
        m.dangling_imports(&RemovalSet::new(["app::keeper"]), &Default::default())
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
        m.dangling_imports(&RemovalSet::new(["app::caller"]), &Default::default())
            .len(),
        1,
        "the only method-call user is removed — the trait import dangles"
    );
    assert!(
        m.dangling_imports(&RemovalSet::new(["app::other"]), &Default::default())
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
        m.dangling_imports(&RemovalSet::new(["app::user"]), &Default::default())
            .len(),
        1,
        "the only `Widget::paint` caller is removed — the type import dangles"
    );
    assert!(
        m.dangling_imports(&RemovalSet::new(["app::other"]), &Default::default())
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
        m.dangling_imports(&RemovalSet::new(["app::user"]), &Default::default())
            .len(),
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
        m.dangling_imports(&RemovalSet::new(["app::user"]), &Default::default())
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
        m.dangling_imports(&RemovalSet::new(["app::keeper"]), &Default::default())
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
    let dangling = m.dangling_imports(&RemovalSet::new(["app::user"]), &Default::default());
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
    let dangling = m.dangling_imports(&RemovalSet::new(["app::tests::t"]), &Default::default());
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
        m.dangling_imports(&RemovalSet::new(["app::user"]), &Default::default())
            .is_empty(),
        "the test fn reaches the outer import via `use super::*` — it stays"
    );
    assert_eq!(
        m.dangling_imports(
            &RemovalSet::new(["app::user", "app::tests::t"]),
            &Default::default()
        )
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
    let dangling = m.dangling_imports(&RemovalSet::new(["app::user"]), &Default::default());
    assert_eq!(
        dangling.len(),
        1,
        "explicit beats glob — the test module's uses no longer shield the outer import"
    );
    assert_eq!(dangling[0].elem.lo, 100);
}

// --- the glob accounting (`dangling_globs`): a `use m::*;` orphaned by a
//     deletion is removed whole-statement, under rules that fail to keeping ---

/// A judgeable glob decl (`use m::*;` at byte `lo` of src/lib.rs) with its
/// resolver facts: the glob_map names and the whole-statement decl span.
fn glob_decl(from: &[&str], to: &[&str], lo: u32, used: &[&str]) -> RefEdge {
    let mut e = glob_edge(from, to);
    e.decl_span = Some(Span {
        file: "src/lib.rs".into(),
        lo,
        hi: lo + 22,
        line: 3,
        from_expansion: false,
    });
    e.glob_used_names = used.iter().map(|s| s.to_string()).collect();
    e
}

/// A `trait_scope` fact (typeck's `used_trait_imports`): the body at `from`
/// needed the `use` item targeting `to` in scope for method resolution.
fn trait_scope_edge(from: &[&str], to: &[&str]) -> RefEdge {
    let mut e = edge(from, to, "K_NOJOIN", false);
    e.trait_scope = true;
    e
}

/// T1: the LeaveDates `feature-state/util.rs` shape — the deleted fn was the
/// module's only consumer of `use dioxus::prelude::*;` (every glob_map name
/// explained by removed edges, nothing surviving) → the glob dangles, as a
/// whole-statement (`glob: true`) deletion.
#[test]
fn dangling_glob_flagged_when_all_uses_removed() {
    let app = frag(
        "app",
        vec![
            item(&["app", "change"], "K_CHG", "fn", Some("mod")),
            item(&["app", "keep"], "K_KEEP", "fn", Some("mod")),
            item(&["app", "local"], "K_LOC", "fn", Some("mod")),
        ],
        vec![
            glob_decl(
                &["app"],
                &["dioxus", "prelude"],
                100,
                &["Event", "EventHandler"],
            ),
            edge(
                &["app", "change"],
                &["dioxus", "prelude", "Event"],
                "K_NOJOIN",
                false,
            ),
            edge(
                &["app", "change"],
                &["dioxus", "prelude", "EventHandler"],
                "K_NOJOIN",
                false,
            ),
            // The survivor only touches a local def — nothing shields the glob.
            edge(&["app", "keep"], &["app", "local"], "K_LOC", false),
        ],
    );
    let m = model(vec![("default", vec![app])]);
    let dangling = m.dangling_imports(&RemovalSet::new(["app::change"]), &Default::default());
    assert_eq!(dangling.len(), 1, "the orphaned glob dangles: {dangling:?}");
    assert!(dangling[0].glob, "whole-statement glob deletion");
    assert_eq!(dangling[0].decl.lo, 100);

    // Same tree, nothing removed: causality keeps the author's glob alone.
    assert!(
        m.dangling_imports(&RemovalSet::new(Vec::<&str>::new()), &Default::default())
            .is_empty(),
        "no removal, no dangle"
    );
}

/// T2/T8 (the rendering-divergence + macro case): a surviving edge whose
/// identity renders under a DIFFERENT path (`dioxus_core_macro::rsx`) but
/// whose final segment matches a glob_map name keeps the glob — name evidence
/// is rendering-independent. `extern_root` isolates R3 from the R6
/// belt-and-braces rule.
#[test]
fn dangling_glob_kept_by_surviving_name_match() {
    let mut rsx_use = edge(
        &["app", "view"],
        &["dioxus_core_macro", "rsx"],
        "K_NOJOIN",
        false,
    );
    rsx_use.extern_root = true; // isolate R3: R6 exempts extern-rooted survivors
    let app = frag(
        "app",
        vec![
            item(&["app", "change"], "K_CHG", "fn", Some("mod")),
            item(&["app", "view"], "K_VIEW", "fn", Some("mod")),
        ],
        vec![
            glob_decl(&["app"], &["dioxus", "prelude"], 100, &["Event", "rsx"]),
            edge(
                &["app", "change"],
                &["dioxus", "prelude", "Event"],
                "K_NOJOIN",
                false,
            ),
            edge(
                &["app", "change"],
                &["dioxus", "prelude", "rsx"],
                "K_NOJOIN",
                false,
            ),
            rsx_use,
        ],
    );
    let m = model(vec![("default", vec![app])]);
    assert!(
        m.dangling_imports(&RemovalSet::new(["app::change"]), &Default::default())
            .is_empty(),
        "the surviving `rsx` use keeps the glob despite its divergent rendering"
    );
}

/// T3: a glob load-bearing only for trait-method syntax — a surviving
/// `trait_scope` fact reaching the glob's module keeps it; when the last
/// trait user is itself removed, the glob dangles (trait-fact causality).
#[test]
fn dangling_glob_respects_trait_scope_facts() {
    let mk = |trait_user_removed: bool| {
        let from: &[&str] = if trait_user_removed {
            &["app", "change"]
        } else {
            &["app", "keep"]
        };
        frag(
            "app",
            vec![
                item(&["app", "change"], "K_CHG", "fn", Some("mod")),
                item(&["app", "keep"], "K_KEEP", "fn", Some("mod")),
            ],
            vec![
                glob_decl(&["app"], &["dioxus", "prelude"], 100, &[]),
                trait_scope_edge(from, &["dioxus", "prelude"]),
            ],
        )
    };
    let m = model(vec![("default", vec![mk(false)])]);
    assert!(
        m.dangling_imports(&RemovalSet::new(["app::change"]), &Default::default())
            .is_empty(),
        "a surviving trait-scope fact keeps the glob (R4)"
    );
    let m = model(vec![("default", vec![mk(true)])]);
    let dangling = m.dangling_imports(&RemovalSet::new(["app::change"]), &Default::default());
    assert_eq!(
        dangling.len(),
        1,
        "the removed fn was the glob's only (trait-scope) user — it dangles"
    );
    assert!(dangling[0].glob);
}

/// T5: glob_map names with no edge evidence at all are typeck probe noise —
/// method selection records every trait candidate a glob supplies (~250 on
/// dioxus's prelude for one real consumer, the 2026-07-08 re-validation
/// finding) — so they carry no keep-weight. Removed-edge causality on the
/// real names still decides.
#[test]
fn dangling_glob_ignores_probe_noise_names() {
    let app = frag(
        "app",
        vec![item(&["app", "change"], "K_CHG", "fn", Some("mod"))],
        vec![
            glob_decl(
                &["app"],
                &["dioxus", "prelude"],
                100,
                // `Event` is really used (and removed); the rest is the
                // resolver's trait-candidate probing — never in HIR.
                &["Event", "HasMouseData", "ReadableExt", "WritableExt"],
            ),
            edge(
                &["app", "change"],
                &["dioxus", "prelude", "Event"],
                "K_NOJOIN",
                false,
            ),
        ],
    );
    let m = model(vec![("default", vec![app])]);
    let dangling = m.dangling_imports(&RemovalSet::new(["app::change"]), &Default::default());
    assert_eq!(
        dangling.len(),
        1,
        "probe-noise names must not shield the orphaned glob (R5): {dangling:?}"
    );
    assert!(dangling[0].glob);
}

/// T5b: the `tracing::Event` shape — a *surviving* `$crate::…`-expanded edge
/// (extern-rooted, from expansion) shares a final segment with a glob_map
/// name. Such a path bypasses local imports by construction, so it carries no
/// name evidence: the glob still dangles. The same edge written in source
/// (not from expansion) keeps it — a glob can supply a crate-rename re-export
/// whose resolution reads extern-rooted.
#[test]
fn dangling_glob_ignores_extern_rooted_expansion_names() {
    let mk = |from_expansion: bool| {
        let mut trace = edge(&["app", "keep"], &["tracing", "Event"], "K_NOJOIN", false);
        trace.extern_root = true;
        trace.span = Some(Span {
            file: "src/lib.rs".into(),
            lo: 900,
            hi: 910,
            line: 40,
            from_expansion,
        });
        frag(
            "app",
            vec![
                item(&["app", "change"], "K_CHG", "fn", Some("mod")),
                item(&["app", "keep"], "K_KEEP", "fn", Some("mod")),
            ],
            vec![
                glob_decl(&["app"], &["dioxus", "prelude"], 100, &["Event"]),
                edge(
                    &["app", "change"],
                    &["dioxus", "prelude", "Event"],
                    "K_NOJOIN",
                    false,
                ),
                trace,
            ],
        )
    };
    let m = model(vec![("default", vec![mk(true)])]);
    assert_eq!(
        m.dangling_imports(&RemovalSet::new(["app::change"]), &Default::default())
            .len(),
        1,
        "a macro-expanded extern-rooted survivor must not shield the glob"
    );
    let m = model(vec![("default", vec![mk(false)])]);
    assert!(
        m.dangling_imports(&RemovalSet::new(["app::change"]), &Default::default())
            .is_empty(),
        "the same path written in source keeps the glob (rename re-export shape)"
    );
}

/// T7: the belt-and-braces rule — a surviving external resolution the model
/// can't attribute to any import blocks glob deletion in its scope; the same
/// edge std-rooted (or covered by the module's own leaf import) does not.
#[test]
fn dangling_glob_blocked_by_unattributable_survivor() {
    let mk = |to_root: &str, with_own_import: bool| {
        let mut refs = vec![
            glob_decl(&["app"], &["dioxus", "prelude"], 100, &["Event"]),
            edge(
                &["app", "change"],
                &["dioxus", "prelude", "Event"],
                "K_NOJOIN",
                false,
            ),
            edge(
                &["app", "keep"],
                &[to_root, "ReadableExt", "read"],
                "K_NOJOIN",
                false,
            ),
        ];
        if with_own_import {
            refs.push(import_edge(
                &["app"],
                &[to_root, "ReadableExt"],
                "K_NOJOIN",
                true,
                400,
            ));
        }
        frag(
            "app",
            vec![
                item(&["app", "change"], "K_CHG", "fn", Some("mod")),
                item(&["app", "keep"], "K_KEEP", "fn", Some("mod")),
            ],
            refs,
        )
    };
    let m = model(vec![("default", vec![mk("dioxus_signals", false)])]);
    assert!(
        m.dangling_imports(&RemovalSet::new(["app::change"]), &Default::default())
            .iter()
            .all(|d| !d.glob),
        "an unattributable survivor blocks the glob (R6)"
    );
    let m = model(vec![("default", vec![mk("std", false)])]);
    assert!(
        m.dangling_imports(&RemovalSet::new(["app::change"]), &Default::default())
            .iter()
            .any(|d| d.glob),
        "std-rooted survivors are exempt from R6"
    );
    let m = model(vec![("default", vec![mk("dioxus_signals", true)])]);
    assert!(
        m.dangling_imports(&RemovalSet::new(["app::change"]), &Default::default())
            .iter()
            .any(|d| d.glob),
        "an explicit own import covers the survivor — explicit-beats-glob (R6)"
    );
}

/// T7c: survivor classes with a *precise* channel of their own must not enter
/// R6 — a receiver-based call (R4's typeck facts), a macro resolution (R3's
/// glob_map names), a generic parameter (never imported). One
/// `tracing::debug!` in a module otherwise R6-blocked every glob in it (its
/// expansion survives as exactly these edge shapes — 2026-07-08 finding).
#[test]
fn dangling_glob_r6_exempts_receiver_macro_and_param_survivors() {
    let mut recv = edge(
        &["app", "keep"],
        &["tracing_core", "Callsite", "metadata"],
        "K_NOJOIN",
        false,
    );
    recv.receiver_resolved = true;
    let mut mac = edge(&["app", "keep"], &["tracing", "debug"], "K_NOJOIN", false);
    mac.to_kind = "macro".into();
    let mut param = edge(
        &["app", "Client", "H"],
        &["app", "Client", "H"],
        "K_NOJOIN",
        false,
    );
    param.to_kind = "param".into();
    let app = frag(
        "app",
        vec![
            item(&["app", "change"], "K_CHG", "fn", Some("mod")),
            item(&["app", "keep"], "K_KEEP", "fn", Some("mod")),
        ],
        vec![
            glob_decl(&["app"], &["dioxus", "prelude"], 100, &["Event"]),
            edge(
                &["app", "change"],
                &["dioxus", "prelude", "Event"],
                "K_NOJOIN",
                false,
            ),
            recv,
            mac,
            param,
        ],
    );
    let m = model(vec![("default", vec![app])]);
    assert_eq!(
        m.dangling_imports(&RemovalSet::new(["app::change"]), &Default::default())
            .len(),
        1,
        "receiver/macro/param survivors must not R6-block the orphaned glob"
    );
}

/// T6/T9: `use super::*` chains bridge the name evidence (a nested module's
/// survivor shields the parent's glob), and per-decl aggregation unions the
/// glob_map across configs (a name surviving only in the tests config still
/// keeps the glob).
#[test]
fn dangling_glob_scope_chains_and_config_union() {
    // A nested module glob-imports the crate root, whose glob supplies Event;
    // the nested survivor uses Event → the root glob is kept.
    let app = frag(
        "app",
        vec![
            item(&["app", "change"], "K_CHG", "fn", Some("mod")),
            item(&["app", "tests"], "K_MOD", "mod", Some("mod")),
            item(&["app", "tests", "t"], "K_T", "fn", Some("mod")),
        ],
        vec![
            glob_decl(&["app"], &["dioxus", "prelude"], 100, &["Event"]),
            glob_edge(&["app", "tests"], &["app"]),
            edge(
                &["app", "change"],
                &["dioxus", "prelude", "Event"],
                "K_NOJOIN",
                false,
            ),
            edge(
                &["app", "tests", "t"],
                &["dioxus", "prelude", "Event"],
                "K_NOJOIN",
                false,
            ),
        ],
    );
    let m = model(vec![("default", vec![app])]);
    assert!(
        m.dangling_imports(&RemovalSet::new(["app::change"]), &Default::default())
            .iter()
            .all(|d| !d.glob),
        "the nested module's surviving use reaches the root glob over the super::* chain"
    );

    // Config union: default's fragment sees only Event (removed with its
    // user); the tests config's glob_map adds Signal, used by a survivor.
    let app_default = frag(
        "app",
        vec![
            item(&["app", "change"], "K_CHG", "fn", Some("mod")),
            item(&["app", "t"], "K_T", "fn", Some("mod")),
        ],
        vec![
            glob_decl(&["app"], &["dioxus", "prelude"], 100, &["Event"]),
            edge(
                &["app", "change"],
                &["dioxus", "prelude", "Event"],
                "K_NOJOIN",
                false,
            ),
        ],
    );
    let app_tests = frag(
        "app",
        vec![
            item(&["app", "change"], "K_CHG2", "fn", Some("mod")),
            item(&["app", "t"], "K_T2", "fn", Some("mod")),
        ],
        vec![
            glob_decl(&["app"], &["dioxus", "prelude"], 100, &["Event", "Signal"]),
            edge(
                &["app", "change"],
                &["dioxus", "prelude", "Event"],
                "K_NOJOIN",
                false,
            ),
            edge(
                &["app", "t"],
                &["dioxus", "prelude", "Signal"],
                "K_NOJOIN",
                false,
            ),
        ],
    );
    let m = model(vec![
        ("default", vec![app_default]),
        ("tests", vec![app_tests]),
    ]);
    assert!(
        m.dangling_imports(&RemovalSet::new(["app::change"]), &Default::default())
            .iter()
            .all(|d| !d.glob),
        "the tests-config survivor of `Signal` keeps the shared decl (config union)"
    );
}

/// T10: `pub use m::*` re-export globs are never flagged (R0), like every
/// other re-export surface.
#[test]
fn dangling_glob_never_flags_reexport() {
    let mut g = glob_decl(&["app"], &["dioxus", "prelude"], 100, &["Event"]);
    g.reexport = true;
    let app = frag(
        "app",
        vec![item(&["app", "change"], "K_CHG", "fn", Some("mod"))],
        vec![
            g,
            edge(
                &["app", "change"],
                &["dioxus", "prelude", "Event"],
                "K_NOJOIN",
                false,
            ),
        ],
    );
    let m = model(vec![("default", vec![app])]);
    assert!(
        m.dangling_imports(&RemovalSet::new(["app::change"]), &Default::default())
            .is_empty(),
        "a re-export glob is not surgery's to touch (R0)"
    );
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
        m.dangling_imports(&RemovalSet::new(["app::user"]), &Default::default())
            .len(),
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
        m.dangling_imports(&RemovalSet::new(["app::user"]), &Default::default())
            .len(),
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
        m.dangling_imports(&RemovalSet::new(["app::user"]), &Default::default())
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
        m.dangling_imports(&RemovalSet::new(["app::user"]), &Default::default())
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
        m.dangling_imports(
            &RemovalSet::new(["app::list::List::dead_sort"]),
            &Default::default()
        )
        .is_empty(),
        "from_iter's `.year()` still needs DateFn in scope — the import stays"
    );

    // Remove the surviving trait-impl edge: now the deleted fn really was the
    // last user, and the import must dangle (guards against over-crediting).
    let mut last_user = lib;
    last_user.references.pop();
    let m = model(vec![("default", vec![last_user])]);
    assert_eq!(
        m.dangling_imports(
            &RemovalSet::new(["app::list::List::dead_sort"]),
            &Default::default()
        )
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

/// The deletion-unmask twin of the narrow guards (the `PopoverMenuClose.is_open`
/// case from the 2026-07-07 LeaveDates validation): removing the LAST reader of
/// a surviving type's private field would trip `dead_code` on the fixed tree —
/// the reader must be vetoed. No veto when the owner goes too, and pre-existing
/// write-only fields (no `newly` edge to attribute) stay the author's business.
#[test]
fn deletion_unmask_flags_last_field_reader() {
    let mk = |with_read: bool| {
        let mut refs = vec![edge(
            &["alpha", "keeper"],
            &["alpha", "Panel"],
            "K_S",
            false,
        )];
        if with_read {
            refs.push(edge(
                &["alpha", "reader"],
                &["alpha", "Panel", "open"],
                "K_F",
                false,
            ));
        }
        frag(
            "alpha",
            vec![
                item(&["alpha", "Panel"], "K_S", "struct", Some("mod")),
                private_item(&["alpha", "Panel", "open"], "K_F", "field", Some("other")),
                item(&["alpha", "reader"], "K_R", "fn", Some("mod")),
                item(&["alpha", "keeper"], "K_K", "fn", Some("mod")),
            ],
            refs,
        )
    };
    let newly = vec!["alpha::reader".to_string()];

    let m = model(vec![("default", vec![mk(true)])]);
    let unmasks = m.deletion_unmasks(&RemovalSet::new(["alpha::reader"]), &newly);
    assert!(
        matches!(
            unmasks.get("alpha::reader"),
            Some(DeletionUnmask::UnreadField { owner, field })
                if owner == "alpha::Panel" && field == "open"
        ),
        "the last field reader is vetoed, attributed to owner+field: {unmasks:?}"
    );

    // Owner deleted in the same trial: nothing survives to fire on.
    let both = vec!["alpha::reader".to_string(), "alpha::Panel".to_string()];
    let m = model(vec![("default", vec![mk(true)])]);
    assert!(
        m.deletion_unmasks(&RemovalSet::new(["alpha::reader", "alpha::Panel"]), &both)
            .is_empty(),
        "co-deleting the owner clears the veto"
    );

    // Pre-existing write-only field: no read edge to kill, nothing to attribute.
    let m = model(vec![("default", vec![mk(false)])]);
    assert!(
        m.deletion_unmasks(&RemovalSet::new(["alpha::reader"]), &newly)
            .is_empty(),
        "a field that was never read is not this deletion's doing"
    );
}

/// The `PasswordData::is_empty` case: deleting a pub `is_empty` out from under
/// a surviving pub `len(&self)` unmasks clippy `len_without_is_empty` on the
/// survivor — vetoed. Deleting the pair together is fine.
#[test]
fn deletion_unmask_flags_is_empty_leaving_len() {
    let mk = || {
        frag(
            "alpha",
            vec![
                item(&["alpha", "List"], "K_L", "struct", Some("mod")),
                method(&["alpha", "List", "len"], "K_LEN", Some("K_L"), "ref", None),
                method(
                    &["alpha", "List", "is_empty"],
                    "K_IE",
                    Some("K_L"),
                    "ref",
                    None,
                ),
            ],
            vec![edge(&["alpha", "keeper"], &["alpha", "List"], "K_L", false)],
        )
    };
    let one = vec!["alpha::List::is_empty".to_string()];
    let m = model(vec![("default", vec![mk()])]);
    let unmasks = m.deletion_unmasks(&RemovalSet::new(["alpha::List::is_empty"]), &one);
    assert!(
        matches!(
            unmasks.get("alpha::List::is_empty"),
            Some(DeletionUnmask::LenWithoutIsEmpty { owner }) if owner == "alpha::List"
        ),
        "deleting is_empty alone is vetoed: {unmasks:?}"
    );

    let both = vec![
        "alpha::List::is_empty".to_string(),
        "alpha::List::len".to_string(),
    ];
    let m = model(vec![("default", vec![mk()])]);
    assert!(
        m.deletion_unmasks(
            &RemovalSet::new(["alpha::List::is_empty", "alpha::List::len"]),
            &both
        )
        .is_empty(),
        "deleting the pair together leaves no survivor to fire on"
    );
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
    // the tests dir never extracted. Both are `+test` cfg variants.
    let alpha_tests = frag_test_cfg(
        "alpha",
        vec![item(
            &["alpha", "render_one"],
            "K_TESTGEN",
            "fn",
            Some("mod"),
        )],
        vec![],
    );
    let beta_tests = frag_test_cfg(
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
    // TestOnly, not Unused: had the global join missed the plain-generation
    // edge, nothing would credit the def at all. And TestOnly, not CrossCrate:
    // the sole referrer is a `+test` unit — production-dead.
    assert_eq!(
        usage_of("alpha::render_one"),
        PubUsage::TestOnly,
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
    let alpha_tests = frag_test_cfg(
        "alpha",
        vec![item(&["alpha", "helper"], "K_TESTGEN", "fn", Some("mod"))],
        vec![],
    );
    let it_crate = frag_target(
        "alpha_it",
        "test",
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
    // Credited (else Unused) — and classified test reach, the referrer being
    // an integration-test crate.
    assert_eq!(usage, PubUsage::TestOnly);
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
/// The reach is credited at identity level instead — with the referrer's
/// provenance (a bench is a test unit), so it classifies TestOnly, and the
/// union retires the lead with correct attribution.
#[test]
fn foreign_reach_credits_unextracted_target_crate() {
    let alpha_default = frag(
        "alpha",
        vec![item(&["alpha", "measured"], "K_PLAIN", "fn", Some("mod"))],
        vec![],
    );
    // The benches config: ONLY the bench crate's fragment — no alpha at all.
    // A bench compiles with `CARGO_TARGET_TMPDIR` set → target_kind "test".
    let bench = frag_target(
        "lookup",
        "test",
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
        PubUsage::TestOnly,
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
    let dangling = m.dangling_imports(&RemovalSet::new(["alpha::measured"]), &Default::default());
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
    let dangling = m.dangling_imports(&RemovalSet::new(["alpha::helper"]), &Default::default());
    assert_eq!(
        dangling.len(),
        1,
        "the cross-config test import must resolve and dangle"
    );
    assert_eq!(dangling[0].elem.lo, 100);
}

// --- Stage-2 config semantics: --target universes + per-crate home config ---

/// A `--target` config is its own DefPathHash universe (different
/// `-C metadata` under `target/<triple>/`): the wasm generation of a def
/// never hash-matches the host generation. Usage *within* the wasm config
/// still joins exactly on its own hashes, and the cross-config verdict
/// unions on the `(crate, def_path)` identity — so a host-unused item whose
/// only caller is wasm-cfg-gated is credited (the `utc_offset` shape from
/// the LeaveDates validation).
#[test]
fn target_config_usage_retires_host_lead_despite_hash_split() {
    let utils_host = frag(
        "utils",
        vec![item(&["utils", "tz_offset"], "K_HOST", "fn", Some("mod"))],
        vec![],
    );
    // Same identity, different universe ⇒ different key; the caller's edge
    // carries the wasm-generation key and resolves inside its own config.
    let utils_wasm = frag(
        "utils",
        vec![item(&["utils", "tz_offset"], "K_WASM", "fn", Some("mod"))],
        vec![],
    );
    let app_wasm = frag(
        "app",
        vec![],
        vec![edge(
            &["app", "clock"],
            &["utils", "tz_offset"],
            "K_WASM",
            false,
        )],
    );
    let host_only = model(vec![("default", vec![utils_host.clone()])]);
    assert_eq!(
        lead_ids(&host_only.union_verdict()),
        ["utils::tz_offset"],
        "host alone must report the lead — the wasm config is what clears it"
    );
    let m = model(vec![
        ("default", vec![utils_host]),
        ("default@wasm32-unknown-unknown", vec![utils_wasm, app_wasm]),
    ]);
    assert!(
        lead_ids(&m.union_verdict()).is_empty(),
        "the wasm config's usage must retire the host lead via the identity union"
    );
}

/// A crate only a `--target` config compiles (a wasm-only member) has that
/// config as its HOME: its pub API is judged there instead of silently
/// escaping judgment because the primary config never extracted it.
#[test]
fn wasm_only_crate_is_judged_via_its_covering_config() {
    let shared = frag(
        "shared",
        vec![item(&["shared", "used"], "K_S", "fn", Some("mod"))],
        vec![],
    );
    let shared_user = frag(
        "app",
        vec![],
        vec![edge(&["app", "main"], &["shared", "used"], "K_S", false)],
    );
    let wasm_only = frag(
        "wasmonly",
        vec![item(&["wasmonly", "dead"], "K_W", "fn", Some("mod"))],
        vec![],
    );
    let m = model(vec![
        ("default", vec![shared, shared_user]),
        ("default@wasm32-unknown-unknown", vec![wasm_only]),
    ]);
    assert_eq!(
        lead_ids(&m.union_verdict()),
        ["wasmonly::dead"],
        "the wasm-only crate's unused pub must be judged by its covering config"
    );
}

/// Integration-test / bench target crates have no home config: their pubs are
/// harness plumbing, never candidates — even when a test-kind config is the
/// only (and thus primary) config.
#[test]
fn test_target_crates_contribute_usage_but_never_candidates() {
    let alpha = frag(
        "alpha",
        vec![item(&["alpha", "helper"], "K_A", "fn", Some("mod"))],
        vec![],
    );
    let itest = frag_target(
        "itest",
        "test",
        vec![item(&["itest", "common_helper"], "K_T", "fn", Some("mod"))],
        vec![edge(&["itest", "case"], &["alpha", "helper"], "K_A", false)],
    );
    let m = model(vec![("tests", vec![alpha, itest])]);
    assert!(
        lead_ids(&m.union_verdict()).is_empty(),
        "alpha::helper is used by the test crate; itest::common_helper must not \
         surface as a candidate at all"
    );
    assert!(
        m.pub_candidates().iter().all(|c| c.krate != "itest"),
        "test-target crates are never candidate sources"
    );
}

// ---------------------------------------------------------------------------
// Call-graph accessors (`enclosing_fn` / `callees_of` / `references_to`) — the
// classifier behind `duplicate-code` resolves a clone instance's byte span to
// its fn identity, then compares callee sets (IR-confirm) and partitions
// inbound references (merge vs delete-dead-copy).
// ---------------------------------------------------------------------------

/// A `fn` ItemFact whose whole-item span is `file[lo..hi]` (the containment
/// surface `enclosing_fn` scans) — `item` fixes every span at `src/lib.rs`
/// 0..10, too coarse for the byte-offset tests.
fn fn_item_at(path: &[&str], key: &str, file: &str, lo: u32, hi: u32) -> ItemFact {
    let mut it = item(path, key, "fn", Some("mod"));
    let s = Span {
        file: file.into(),
        lo,
        hi,
        line: 1,
        from_expansion: false,
    };
    it.span = Some(s.clone());
    it.full_span = Some(s);
    it
}

#[test]
fn enclosing_fn_picks_innermost_by_byte_containment() {
    let app = frag(
        "app",
        vec![
            fn_item_at(&["app", "outer"], "K_OUT", "src/a.rs", 0, 100),
            fn_item_at(&["app", "outer", "inner"], "K_IN", "src/a.rs", 40, 60),
        ],
        vec![],
    );
    let m = model(vec![("default", vec![app])]);
    let a = std::path::Path::new("src/a.rs");
    // Inside both → innermost (smallest interval) wins.
    assert_eq!(m.enclosing_fn(a, 50).unwrap().identity, "app::outer::inner");
    // Inside outer only.
    assert_eq!(m.enclosing_fn(a, 20).unwrap().identity, "app::outer");
    // `hi` is exclusive: 60 is past inner, still within outer.
    assert_eq!(m.enclosing_fn(a, 60).unwrap().identity, "app::outer");
    // 100 is past outer's close → nothing.
    assert!(m.enclosing_fn(a, 100).is_none());
    // Off the end, and a file with no fns.
    assert!(m.enclosing_fn(a, 500).is_none());
    assert!(
        m.enclosing_fn(std::path::Path::new("src/b.rs"), 50)
            .is_none()
    );
    // The whole-item span rides along.
    let fs = m.enclosing_fn(a, 50).unwrap().full_span;
    assert_eq!((fs.lo, fs.hi, fs.file.as_str()), (40, 60, "src/a.rs"));
}

#[test]
fn enclosing_fn_skips_expansion_and_synthetic_defs() {
    let mut macro_fn = fn_item_at(&["app", "generated"], "K_M", "src/m.rs", 0, 50);
    macro_fn.full_span.as_mut().unwrap().from_expansion = true;
    let mut synthetic = fn_item_at(&["app", "synth"], "K_S", "src/m.rs", 60, 90);
    synthetic.full_span = None;
    let real = fn_item_at(&["app", "real"], "K_R", "src/m.rs", 100, 120);
    let app = frag("app", vec![macro_fn, synthetic, real], vec![]);
    let m = model(vec![("default", vec![app])]);
    let f = std::path::Path::new("src/m.rs");
    // Inside the macro-generated span → skipped (no editable owner).
    assert!(m.enclosing_fn(f, 25).is_none());
    // Inside the synthetic (spanless full_span) fn → skipped.
    assert!(m.enclosing_fn(f, 75).is_none());
    // The real fn resolves.
    assert_eq!(m.enclosing_fn(f, 110).unwrap().identity, "app::real");
}

#[test]
fn enclosing_fn_sees_secondary_config_only_defs() {
    // A fn in both configs (identical span — primary wins) plus one only the
    // `test` config extracted (a `#[cfg(test)]` fn).
    let shared = || fn_item_at(&["app", "shared"], "K_SH", "src/x.rs", 0, 20);
    let default = frag("app", vec![shared()], vec![]);
    let test = frag_target(
        "app",
        "test",
        vec![
            shared(),
            fn_item_at(&["app", "only_test"], "K_ONLYTEST", "src/x.rs", 30, 50),
        ],
        vec![],
    );
    let m = model(vec![("default", vec![default]), ("test", vec![test])]);
    let x = std::path::Path::new("src/x.rs");
    assert_eq!(m.enclosing_fn(x, 10).unwrap().identity, "app::shared");
    assert_eq!(m.enclosing_fn(x, 40).unwrap().identity, "app::only_test");
}

#[test]
fn callees_union_across_configs_by_identity() {
    // caller calls `a` under default and `b` under test — the union is both.
    let defs = || {
        vec![
            item(&["app", "caller"], "K_C", "fn", Some("mod")),
            item(&["app", "a"], "K_A", "fn", Some("mod")),
            item(&["app", "b"], "K_B", "fn", Some("mod")),
        ]
    };
    let default = frag(
        "app",
        defs(),
        vec![edge(&["app", "caller"], &["app", "a"], "K_A", false)],
    );
    let test = frag_target(
        "app",
        "test",
        defs(),
        vec![edge(&["app", "caller"], &["app", "b"], "K_B", false)],
    );
    let m = model(vec![("default", vec![default]), ("test", vec![test])]);
    let set = m.callees_of("app::caller");
    let targets: Vec<&str> = set.iter().map(|c| c.target.as_str()).collect();
    assert_eq!(
        targets,
        ["app::a", "app::b"],
        "callees union across configs"
    );
}

#[test]
fn callees_translate_unextracted_generation_through_global_index() {
    // `dep::thing` is extracted only by default (plain generation). The test
    // config's edge carries that plain to_key but has no local dep def — the
    // global index still resolves it to the identity.
    let dep = frag(
        "dep",
        vec![item(&["dep", "thing"], "K_PLAIN", "fn", Some("mod"))],
        vec![],
    );
    let app_default = frag(
        "app",
        vec![item(&["app", "caller"], "K_C", "fn", Some("mod"))],
        vec![],
    );
    let app_test = frag_target(
        "app",
        "test",
        vec![item(&["app", "caller"], "K_C", "fn", Some("mod"))],
        vec![edge(
            &["app", "caller"],
            &["dep", "thing"],
            "K_PLAIN",
            false,
        )],
    );
    let m = model(vec![
        ("default", vec![dep, app_default]),
        ("test", vec![app_test]),
    ]);
    let callees = m.callees_of("app::caller");
    let thing = callees.iter().find(|c| c.target == "dep::thing");
    assert!(
        thing.is_some(),
        "global index resolves the plain-generation edge"
    );
    assert!(thing.unwrap().resolved, "a workspace identity is resolved");
}

#[test]
fn callees_aggregate_nested_defs_segment_wise() {
    // `foo` and its nested `foo::helper` both contribute; the sibling
    // `foo_bar` (a string prefix, NOT a `::` segment) must not.
    let app = frag(
        "app",
        vec![
            item(&["app", "foo"], "K_FOO", "fn", Some("mod")),
            item(&["app", "foo", "helper"], "K_H", "fn", Some("fn")),
            item(&["app", "foo_bar"], "K_FB", "fn", Some("mod")),
            item(&["app", "a"], "K_A", "fn", Some("mod")),
            item(&["app", "b"], "K_B", "fn", Some("mod")),
            item(&["app", "c"], "K_C", "fn", Some("mod")),
        ],
        vec![
            edge(&["app", "foo"], &["app", "a"], "K_A", false),
            edge(&["app", "foo", "helper"], &["app", "b"], "K_B", false),
            edge(&["app", "foo_bar"], &["app", "c"], "K_C", false),
        ],
    );
    let m = model(vec![("default", vec![app])]);
    let set = m.callees_of("app::foo");
    let targets: Vec<&str> = set.iter().map(|c| c.target.as_str()).collect();
    assert_eq!(
        targets,
        ["app::a", "app::b"],
        "foo + nested foo::helper, but not sibling foo_bar"
    );
}

#[test]
fn callees_keep_foreign_targets_by_display_path() {
    // Two fns identical but for which out-of-workspace fn they call: their
    // callee sets must differ (the IR-confirm contract).
    let c1 = edge(&["app", "foo1"], &["std", "fmt", "one"], "K_STD1", false);
    let c2 = edge(&["app", "foo2"], &["std", "fmt", "two"], "K_STD2", false);
    let app = frag(
        "app",
        vec![
            item(&["app", "foo1"], "K_F1", "fn", Some("mod")),
            item(&["app", "foo2"], "K_F2", "fn", Some("mod")),
        ],
        vec![c1, c2],
    );
    let m = model(vec![("default", vec![app])]);
    let s1 = m.callees_of("app::foo1");
    let s2 = m.callees_of("app::foo2");
    let only1: Vec<_> = s1.iter().collect();
    assert_eq!(only1.len(), 1);
    assert_eq!(only1[0].target, "std::fmt::one");
    assert!(!only1[0].resolved, "out-of-workspace target is unresolved");
    assert_ne!(s1, s2, "differing foreign callees ⇒ unequal sets");
}

#[test]
fn callees_exclude_import_edges() {
    // A real call is a callee; imports, globs, and trait-scope facts are not.
    let import = edge(&["app", "foo"], &["app", "b"], "K_B", true);
    let mut glob = edge(&["app", "foo"], &["app", "m"], "K_M", true);
    glob.glob = true;
    let mut trait_scope = edge(&["app", "foo"], &["app", "Tr"], "K_TR", false);
    trait_scope.trait_scope = true;
    let app = frag(
        "app",
        vec![
            item(&["app", "foo"], "K_FOO", "fn", Some("mod")),
            item(&["app", "a"], "K_A", "fn", Some("mod")),
            item(&["app", "b"], "K_B", "fn", Some("mod")),
            item(&["app", "m"], "K_M", "mod", Some("mod")),
            item(&["app", "Tr"], "K_TR", "trait", Some("mod")),
        ],
        vec![
            edge(&["app", "foo"], &["app", "a"], "K_A", false),
            import,
            glob,
            trait_scope,
        ],
    );
    let m = model(vec![("default", vec![app])]);
    let set = m.callees_of("app::foo");
    let targets: Vec<&str> = set.iter().map(|c| c.target.as_str()).collect();
    assert_eq!(targets, ["app::a"], "only the real call is a callee");
}

#[test]
fn callees_exclude_param_edges() {
    // Argument-position `impl Trait` emits a `param`-kind edge to a fn-scoped
    // synthetic parameter (`<fn>::impl Trait`) — not a call, and fn-local, so
    // two otherwise-identical fns would get UNEQUAL callee sets and the merge
    // family would spuriously withhold them. Excluding `param` edges keeps the
    // IR-confirm equality faithful: the two fns below share one real call and
    // differ only in their own param edge, so their callee sets must match.
    let mut left_param = edge(
        &["app", "left"],
        &["app", "left", "impl Into<String>"],
        "K_LP",
        false,
    );
    left_param.to_kind = "param".into();
    let mut right_param = edge(
        &["app", "right"],
        &["app", "right", "impl Into<String>"],
        "K_RP",
        false,
    );
    right_param.to_kind = "param".into();
    let app = frag(
        "app",
        vec![
            item(&["app", "left"], "K_L", "fn", Some("mod")),
            item(&["app", "right"], "K_R", "fn", Some("mod")),
            item(&["app", "shared"], "K_S", "fn", Some("mod")),
        ],
        vec![
            edge(&["app", "left"], &["app", "shared"], "K_S", false),
            left_param,
            edge(&["app", "right"], &["app", "shared"], "K_S", false),
            right_param,
        ],
    );
    let m = model(vec![("default", vec![app])]);
    assert_eq!(
        m.callees_of("app::left"),
        m.callees_of("app::right"),
        "param edges excluded → the two fns' callee sets match (mergeable)"
    );
    let left_set = m.callees_of("app::left");
    let left: Vec<&str> = left_set.iter().map(|c| c.target.as_str()).collect();
    assert_eq!(left, ["app::shared"], "the param target is not a callee");
}

#[test]
fn references_to_carry_spans_and_flags() {
    // target referenced three ways: a spanned call site, an import, a
    // signature projection.
    let mut call = edge(&["app", "caller"], &["app", "target"], "K_T", false);
    call.span = Some(Span {
        file: "src/c.rs".into(),
        lo: 5,
        hi: 8,
        line: 2,
        from_expansion: false,
    });
    let import = edge(&["app", "user"], &["app", "target"], "K_T", true);
    let mut sig = edge(&["app", "api"], &["app", "target"], "K_T", false);
    sig.in_signature = true;
    let app = frag(
        "app",
        vec![
            item(&["app", "target"], "K_T", "fn", Some("mod")),
            item(&["app", "caller"], "K_CA", "fn", Some("mod")),
            item(&["app", "user"], "K_U", "fn", Some("mod")),
            item(&["app", "api"], "K_AP", "fn", Some("mod")),
        ],
        vec![call, import, sig],
    );
    let m = model(vec![("default", vec![app])]);
    let refs = m.references_to("app::target");
    let c = refs.iter().find(|r| r.from == "app::caller").unwrap();
    assert_eq!(c.span.as_ref().map(|s| (s.lo, s.hi)), Some((5, 8)));
    assert!(!c.import && !c.in_signature);
    assert!(refs.iter().find(|r| r.from == "app::user").unwrap().import);
    assert!(
        refs.iter()
            .find(|r| r.from == "app::api")
            .unwrap()
            .in_signature
    );
}

#[test]
fn references_to_dedup_cfg_variants() {
    // The same call site under the plain and +test generations (different
    // to_key, same identity + span) is one inbound ref.
    let make = |to_key: &str| {
        let mut e = edge(&["app", "caller"], &["app", "target"], to_key, false);
        e.span = Some(Span {
            file: "src/d.rs".into(),
            lo: 1,
            hi: 4,
            line: 1,
            from_expansion: false,
        });
        e
    };
    let default = frag(
        "app",
        vec![
            item(&["app", "target"], "K_PLAIN", "fn", Some("mod")),
            item(&["app", "caller"], "K_CA", "fn", Some("mod")),
        ],
        vec![make("K_PLAIN")],
    );
    let test = frag_target(
        "app",
        "test",
        vec![
            item(&["app", "target"], "K_TEST", "fn", Some("mod")),
            item(&["app", "caller"], "K_CA", "fn", Some("mod")),
        ],
        vec![make("K_TEST")],
    );
    let m = model(vec![("default", vec![default]), ("test", vec![test])]);
    let refs = m.references_to("app::target");
    assert_eq!(refs.len(), 1, "cfg variants of one call site dedup");
    assert_eq!(refs[0].from, "app::caller");
}

#[test]
fn references_to_include_foreign_config_leg() {
    // `dep::thing` is extracted only by default; its ONLY reference comes from
    // the test config, which has no dep def — the inbound edge still lands via
    // the global index (the ForeignReach shape).
    let dep = frag(
        "dep",
        vec![item(&["dep", "thing"], "K_PLAIN", "fn", Some("mod"))],
        vec![],
    );
    let app_test = frag_target(
        "app",
        "test",
        vec![item(&["app", "caller"], "K_CA", "fn", Some("mod"))],
        vec![edge(
            &["app", "caller"],
            &["dep", "thing"],
            "K_PLAIN",
            false,
        )],
    );
    let m = model(vec![("default", vec![dep]), ("test", vec![app_test])]);
    let refs = m.references_to("dep::thing");
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].from, "app::caller");
}

#[test]
fn references_to_join_build_fragments_by_path_fallback() {
    // A build-script edge carries a Build-mode key that joins nothing by hash;
    // the display-path fallback still lands it on the target.
    let app = frag(
        "app",
        vec![item(&["app", "thing"], "K_CHECK", "fn", Some("mod"))],
        vec![],
    );
    let build = build_frag(vec![edge(
        &["build_script_build", "main"],
        &["app", "thing"],
        "K_BUILD_MODE",
        false,
    )]);
    let m = model(vec![("default", vec![app, build])]);
    let refs = m.references_to("app::thing");
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].from, "build_script_build::main");
}

#[test]
fn unreferenced_fn_has_no_inbound_refs() {
    let app = frag(
        "app",
        vec![item(&["app", "lonely"], "K_L", "fn", Some("mod"))],
        vec![],
    );
    let m = model(vec![("default", vec![app])]);
    assert!(m.references_to("app::lonely").is_empty());
    assert!(m.callees_of("app::lonely").is_empty());
}

// ---------------------------------------------------------------------------
// `loaded_files` — the substrate `orphan-file` judges. The assembler's only job
// is the union; the *meaning* of each path is rustc's, pinned by the tier-1
// probe (`extractor/tests/probe.rs`).
// ---------------------------------------------------------------------------

/// A fragment is one compilation unit, so a `#[cfg(test)] mod tests;` file is
/// opened only by the `cargo test` config. Judging any single config would call
/// it dead; the union is what makes the verdict honest.
#[test]
fn loaded_files_union_across_the_config_matrix() {
    let build = frag_files("demo", &["/w/src/lib.rs"]);
    let test = frag_files("demo", &["/w/src/lib.rs", "/w/src/tests.rs"]);
    let m = model(vec![("default", vec![build]), ("tests", vec![test])]);
    assert_eq!(
        m.loaded_files().into_iter().collect::<Vec<_>>(),
        ["/w/src/lib.rs", "/w/src/tests.rs"]
    );
}

/// Each member contributes its own files; the union spans crates as well as
/// configs (one workspace-global answer, like the def hash join).
#[test]
fn loaded_files_union_across_crates() {
    let alpha = frag_files("alpha", &["/w/alpha/src/lib.rs"]);
    let beta = frag_files("beta", &["/w/beta/src/lib.rs"]);
    let m = model(vec![("default", vec![alpha, beta])]);
    assert_eq!(
        m.loaded_files().into_iter().collect::<Vec<_>>(),
        ["/w/alpha/src/lib.rs", "/w/beta/src/lib.rs"]
    );
}

/// A build-script fragment's files count: a `build.rs` may `include!` a source
/// file, and that file is then genuinely compiled.
#[test]
fn loaded_files_include_build_script_fragments() {
    let app = frag_files("app", &["/w/src/lib.rs"]);
    let mut build = build_frag(vec![]);
    build.loaded_files = vec!["/w/build.rs".into(), "/w/src/shared.rs".into()];
    let m = model(vec![("default", vec![app, build])]);
    assert!(m.loaded_files().contains("/w/src/shared.rs"));
}

/// The zero-fragment guard's substrate: a scoped config (`cargo build -p alpha`)
/// leaves `beta` uncompiled, so `beta` emits nothing. Its absence here is what
/// stops a consumer from reading "no files loaded" as "every file is dead".
/// Build-script carriers are not crates of the assembly and must not appear.
#[test]
fn fragment_crates_reports_only_crates_that_emitted() {
    let alpha = frag_files("alpha", &["/w/alpha/src/lib.rs"]);
    let build = build_frag(vec![]);
    let m = model(vec![("default", vec![alpha, build])]);
    let crates: Vec<&str> = m.fragment_crates().into_iter().collect();
    assert_eq!(crates, ["alpha"]);
    assert!(
        !crates.contains(&"build_script_build"),
        "a build-script carrier is not a member crate"
    );
}

// --- test_scaffolding: the exclusive-scaffolding gate for TestOnly deletion ---

/// Builds the render_one-shaped workspace: `alpha::embalmed` reached only by
/// `beta::tests::t`, which also calls the test-local helper only it uses.
/// Both are exclusive scaffolding — the whole closure clears, helper included
/// (the mutual-recursion arm of the fixpoint).
#[test]
fn test_scaffolding_clears_exclusive_closure() {
    let alpha = frag(
        "alpha",
        vec![item(&["alpha", "embalmed"], "K_E", "fn", Some("mod"))],
        vec![],
    );
    let beta_tests = frag_test_cfg(
        "beta",
        vec![
            item(&["beta", "tests", "t"], "K_T", "fn", Some("mod")),
            item(&["beta", "tests", "helper"], "K_H", "fn", Some("mod")),
        ],
        vec![
            edge_ext(
                &["beta", "tests", "t"],
                &["alpha", "embalmed"],
                "K_E",
                false,
                true,
            ),
            edge(
                &["beta", "tests", "t"],
                &["beta", "tests", "helper"],
                "K_H",
                false,
            ),
            edge_ext(
                &["beta", "tests", "helper"],
                &["alpha", "embalmed"],
                "K_E",
                false,
                true,
            ),
        ],
    );
    let m = model(vec![("default", vec![alpha, beta_tests])]);
    let sc = m.test_scaffolding(
        &RemovalSet::new(std::iter::empty::<&str>()),
        &["alpha::embalmed".to_string()],
    );
    match &sc.per_target["alpha::embalmed"] {
        ScaffoldVerdict::Cleared { scaffolding } => {
            let ids: Vec<&str> = scaffolding.iter().map(|s| s.id.as_str()).collect();
            assert_eq!(ids, ["beta::tests::helper", "beta::tests::t"]);
        }
        other => panic!("expected Cleared, got {other:?}"),
    }
}

/// The test fn also asserts on surviving code: NOT exclusive scaffolding —
/// deleting it would drop real coverage, so the target is blocked and the
/// blocker names the surviving reach.
#[test]
fn test_scaffolding_blocks_on_surviving_reach() {
    let alpha = frag(
        "alpha",
        vec![
            item(&["alpha", "embalmed"], "K_E", "fn", Some("mod")),
            item(&["alpha", "kept"], "K_K", "fn", Some("mod")),
        ],
        vec![],
    );
    let beta_tests = frag_test_cfg(
        "beta",
        vec![item(&["beta", "tests", "t"], "K_T", "fn", Some("mod"))],
        vec![
            edge_ext(
                &["beta", "tests", "t"],
                &["alpha", "embalmed"],
                "K_E",
                false,
                true,
            ),
            edge_ext(
                &["beta", "tests", "t"],
                &["alpha", "kept"],
                "K_K",
                false,
                true,
            ),
        ],
    );
    let m = model(vec![("default", vec![alpha, beta_tests])]);
    let sc = m.test_scaffolding(
        &RemovalSet::new(std::iter::empty::<&str>()),
        &["alpha::embalmed".to_string()],
    );
    match &sc.per_target["alpha::embalmed"] {
        ScaffoldVerdict::Blocked(b) => {
            assert_eq!(b.test, "beta::tests::t");
            assert!(
                matches!(&b.reason, BlockReason::ReachesSurviving { to } if to == "alpha::kept"),
                "wrong reason: {:?}",
                b.reason
            );
        }
        other => panic!("expected Blocked, got {other:?}"),
    }
}

/// A shared fixture: the scaffold-candidate helper is also used by a test
/// that has nothing to do with the target. The helper is anchored by that
/// survivor, which demotes it — and the demotion propagates to the test fn
/// leaning on it, blocking the target (the shrinking-fixpoint arm).
#[test]
fn test_scaffolding_blocks_on_shared_fixture() {
    let alpha = frag(
        "alpha",
        vec![
            item(&["alpha", "embalmed"], "K_E", "fn", Some("mod")),
            item(&["alpha", "kept"], "K_K", "fn", Some("mod")),
        ],
        vec![],
    );
    let beta_tests = frag_test_cfg(
        "beta",
        vec![
            item(&["beta", "tests", "t1"], "K_T1", "fn", Some("mod")),
            item(&["beta", "tests", "t2"], "K_T2", "fn", Some("mod")),
            item(&["beta", "tests", "fixture"], "K_F", "fn", Some("mod")),
        ],
        vec![
            edge_ext(
                &["beta", "tests", "t1"],
                &["alpha", "embalmed"],
                "K_E",
                false,
                true,
            ),
            edge(
                &["beta", "tests", "t1"],
                &["beta", "tests", "fixture"],
                "K_F",
                false,
            ),
            // The unrelated survivor: t2 exercises kept through the fixture.
            edge(
                &["beta", "tests", "t2"],
                &["beta", "tests", "fixture"],
                "K_F",
                false,
            ),
            edge_ext(
                &["beta", "tests", "t2"],
                &["alpha", "kept"],
                "K_K",
                false,
                true,
            ),
        ],
    );
    let m = model(vec![("default", vec![alpha, beta_tests])]);
    let sc = m.test_scaffolding(
        &RemovalSet::new(std::iter::empty::<&str>()),
        &["alpha::embalmed".to_string()],
    );
    match &sc.per_target["alpha::embalmed"] {
        ScaffoldVerdict::Blocked(b) => {
            assert_eq!(b.test, "beta::tests::t1");
            assert!(
                matches!(&b.reason, BlockReason::ReachesSurviving { to } if to == "beta::tests::fixture"),
                "wrong reason: {:?}",
                b.reason
            );
        }
        other => panic!("expected Blocked, got {other:?}"),
    }
}

/// The target's direct referrer is a shared helper: a test fn outside the
/// closure still calls it. The universe never chases *inbound* referrers, so
/// the helper is anchored (`KeptBySurvivor`) and that — not a reach — is the
/// reported blocker.
#[test]
fn test_scaffolding_reports_kept_by_survivor_blocker() {
    let alpha = frag(
        "alpha",
        vec![item(&["alpha", "embalmed"], "K_E", "fn", Some("mod"))],
        vec![],
    );
    let beta_tests = frag_test_cfg(
        "beta",
        vec![
            item(&["beta", "tests", "check"], "K_C", "fn", Some("mod")),
            item(&["beta", "tests", "t2"], "K_T2", "fn", Some("mod")),
        ],
        vec![
            edge_ext(
                &["beta", "tests", "check"],
                &["alpha", "embalmed"],
                "K_E",
                false,
                true,
            ),
            edge(
                &["beta", "tests", "t2"],
                &["beta", "tests", "check"],
                "K_C",
                false,
            ),
        ],
    );
    let m = model(vec![("default", vec![alpha, beta_tests])]);
    let sc = m.test_scaffolding(
        &RemovalSet::new(std::iter::empty::<&str>()),
        &["alpha::embalmed".to_string()],
    );
    match &sc.per_target["alpha::embalmed"] {
        ScaffoldVerdict::Blocked(b) => {
            assert_eq!(b.test, "beta::tests::check");
            assert!(
                matches!(&b.reason, BlockReason::KeptBySurvivor { from } if from == "beta::tests::t2"),
                "wrong reason: {:?}",
                b.reason
            );
        }
        other => panic!("expected Blocked, got {other:?}"),
    }
}

/// A macro-generated referencing test (the `#[rstest]`/`test_case` shape: the
/// fn's span is from-expansion) has no safe auto-delete surface — the target
/// is blocked `NotDeletable`, never half-deleted.
#[test]
fn test_scaffolding_blocks_on_macro_generated_test() {
    let alpha = frag(
        "alpha",
        vec![item(&["alpha", "embalmed"], "K_E", "fn", Some("mod"))],
        vec![],
    );
    let mut t = item(&["beta", "tests", "t"], "K_T", "fn", Some("mod"));
    t.span.as_mut().unwrap().from_expansion = true;
    let beta_tests = frag_test_cfg(
        "beta",
        vec![t],
        vec![edge_ext(
            &["beta", "tests", "t"],
            &["alpha", "embalmed"],
            "K_E",
            false,
            true,
        )],
    );
    let m = model(vec![("default", vec![alpha, beta_tests])]);
    let sc = m.test_scaffolding(
        &RemovalSet::new(std::iter::empty::<&str>()),
        &["alpha::embalmed".to_string()],
    );
    match &sc.per_target["alpha::embalmed"] {
        ScaffoldVerdict::Blocked(b) => {
            assert_eq!(b.test, "beta::tests::t");
            assert!(
                matches!(&b.reason, BlockReason::NotDeletable),
                "wrong reason: {:?}",
                b.reason
            );
        }
        other => panic!("expected Blocked, got {other:?}"),
    }
}

/// The harness-main trap, pinned at the rule: the `--test` harness's
/// generated `fn main` is a *synthetic* def sharing the real `main`'s
/// identity and referencing every `#[test]` fn. Its edge is compiler
/// plumbing, not an anchor — the closure must still clear.
#[test]
fn test_scaffolding_ignores_synthetic_harness_referrer() {
    let alpha = frag(
        "alpha",
        vec![item(&["alpha", "embalmed"], "K_E", "fn", Some("mod"))],
        vec![],
    );
    // The real bin `main` — makes `beta::main` a production identity, exactly
    // the collision the synthetic harness main hides behind.
    let beta = frag_target(
        "beta",
        "bin",
        vec![item(&["beta", "main"], "K_M_REAL", "fn", Some("mod"))],
        vec![],
    );
    let mut harness_main = item(&["beta", "main"], "K_M", "fn", Some("mod"));
    harness_main.span = None; // spanless ⇒ synthetic
    harness_main.full_span = None;
    let mut harness_edge = edge(&["beta", "main"], &["beta", "tests", "t"], "K_T", false);
    harness_edge.from_key = "K_M".into();
    let beta_tests = frag_test_cfg(
        "beta",
        vec![
            harness_main,
            item(&["beta", "tests", "t"], "K_T", "fn", Some("mod")),
        ],
        vec![
            edge_ext(
                &["beta", "tests", "t"],
                &["alpha", "embalmed"],
                "K_E",
                false,
                true,
            ),
            harness_edge,
        ],
    );
    let m = model(vec![("default", vec![alpha, beta, beta_tests])]);
    let sc = m.test_scaffolding(
        &RemovalSet::new(std::iter::empty::<&str>()),
        &["alpha::embalmed".to_string()],
    );
    match &sc.per_target["alpha::embalmed"] {
        ScaffoldVerdict::Cleared { scaffolding } => {
            let ids: Vec<&str> = scaffolding.iter().map(|s| s.id.as_str()).collect();
            assert_eq!(ids, ["beta::tests::t"]);
        }
        other => panic!("expected Cleared, got {other:?}"),
    }
}

/// One test fn exercises two targets dying in the same round: both clear,
/// each listing the same scaffold — the shape the cascade must commit once.
#[test]
fn test_scaffolding_shared_scaffold_clears_sibling_targets() {
    let alpha = frag(
        "alpha",
        vec![
            item(&["alpha", "embalmed_a"], "K_A", "fn", Some("mod")),
            item(&["alpha", "embalmed_b"], "K_B", "fn", Some("mod")),
        ],
        vec![],
    );
    let beta_tests = frag_test_cfg(
        "beta",
        vec![item(&["beta", "tests", "t"], "K_T", "fn", Some("mod"))],
        vec![
            edge_ext(
                &["beta", "tests", "t"],
                &["alpha", "embalmed_a"],
                "K_A",
                false,
                true,
            ),
            edge_ext(
                &["beta", "tests", "t"],
                &["alpha", "embalmed_b"],
                "K_B",
                false,
                true,
            ),
        ],
    );
    let m = model(vec![("default", vec![alpha, beta_tests])]);
    let sc = m.test_scaffolding(
        &RemovalSet::new(std::iter::empty::<&str>()),
        &[
            "alpha::embalmed_a".to_string(),
            "alpha::embalmed_b".to_string(),
        ],
    );
    for target in ["alpha::embalmed_a", "alpha::embalmed_b"] {
        match &sc.per_target[target] {
            ScaffoldVerdict::Cleared { scaffolding } => {
                let ids: Vec<&str> = scaffolding.iter().map(|s| s.id.as_str()).collect();
                assert_eq!(ids, ["beta::tests::t"], "{target}");
            }
            other => panic!("expected Cleared for {target}, got {other:?}"),
        }
    }
}
