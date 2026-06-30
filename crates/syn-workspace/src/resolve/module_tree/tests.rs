//! Module-tree assembly and occurrence-resolution tests, split out of `mod.rs`.

use super::*;

fn manifest_dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

#[test]
fn flat_lib_collects_top_level_items() {
    let root = build_crate_tree(&manifest_dir("flat_lib"), "flat_lib").expect("build");
    let names: Vec<_> = root.items.iter().map(|i| i.name.as_str()).collect();
    assert!(names.contains(&"public_fn"), "got {names:?}");
    assert!(names.contains(&"PrivateStruct"), "got {names:?}");
}

#[test]
fn include_macro_splices_generated_items_and_marks_file() {
    // A literal `include!("generated.rs")` (Tier 1 — no build script needed):
    // the generated file's items must land in the including module's scope, and
    // the file must be recorded as generated.
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("src");
    std::fs::create_dir(&src).unwrap();
    std::fs::write(
        src.join("lib.rs"),
        "include!(\"generated.rs\");\npub fn uses_gen() { helper(); }\n",
    )
    .unwrap();
    // The generated code references an external crate by a multi-segment path —
    // the kind of reference unused-deps relies on seeing.
    std::fs::write(
        src.join("generated.rs"),
        "pub fn helper() { external_dep::do_thing(); }\n",
    )
    .unwrap();

    let root = build_crate_tree(dir.path(), "demo").expect("build");

    let names: Vec<_> = root.items.iter().map(|i| i.name.as_str()).collect();
    assert!(
        names.contains(&"helper"),
        "spliced generated item missing from {names:?}"
    );
    assert!(names.contains(&"uses_gen"), "{names:?}");
    assert!(
        root.generated_files
            .iter()
            .any(|p| p.ends_with("generated.rs")),
        "generated.rs not recorded: {:?}",
        root.generated_files
    );
    // The generated code's outgoing reference is folded into the model, so a dep
    // used only from generated code is visible (no unused-deps false positive).
    let refs: Vec<String> = root.references().map(|p| p.segments().join("::")).collect();
    assert!(
        refs.iter().any(|r| r.starts_with("external_dep")),
        "expected the generated file's external_dep reference, got {refs:?}"
    );
}

#[test]
fn mod_decl_walks_to_adjacent_file() {
    let root = build_crate_tree(&manifest_dir("nested_modules"), "nested_modules").expect("build");
    let sub = root
        .submodules
        .iter()
        .find(|m| m.name == "sub")
        .expect("sub mod");
    let item_names: Vec<_> = sub.items.iter().map(|i| i.name.as_str()).collect();
    assert!(item_names.contains(&"child_item"), "got {item_names:?}");
    assert_eq!(sub.canonical.display(), "nested_modules::sub");
}

#[test]
fn mod_decl_walks_to_dir_mod_rs() {
    let root = build_crate_tree(&manifest_dir("nested_modules"), "nested_modules").expect("build");
    let dir_mod = root
        .submodules
        .iter()
        .find(|m| m.name == "dir_mod")
        .expect("dir_mod");
    let item_names: Vec<_> = dir_mod.items.iter().map(|i| i.name.as_str()).collect();
    assert!(item_names.contains(&"in_dir_mod"), "got {item_names:?}");
}

#[test]
fn target_root_resolves_sibling_submodule() {
    // Regression guard (follow-up to #29): a target root whose filename
    // stem is not lib/main/mod — e.g. an integration-test root
    // `tests/it.rs` — owns its *containing* directory, so `mod common;`
    // resolves to the sibling `common/mod.rs`, not `it/common/mod.rs`.
    // `walk.rs` builds every target this way; before the fix, every test /
    // example / bench / build-script root silently dropped its submodules.
    let dir = manifest_dir("nested_modules").join("target_root");
    let root = build_module_from_file(
        &dir.join("it.rs"),
        &dir,
        "it".to_string(),
        ResolvedPath::new(["it".to_string()]),
        Visibility::Public,
        &default_markers(),
        IncludeCtx::none(),
    )
    .expect("build target root");
    let common = root
        .submodules
        .iter()
        .find(|m| m.name == "common")
        .expect("`mod common;` in a target root must resolve to the sibling common/mod.rs");
    let item_names: Vec<_> = common.items.iter().map(|i| i.name.as_str()).collect();
    assert!(item_names.contains(&"helper"), "got {item_names:?}");
    assert!(
        root.broken_mod_decls.is_empty(),
        "no broken mod decls expected, got {:?}",
        root.broken_mod_decls
    );
}

#[test]
fn file_module_owns_subdir() {
    // `src/sub.rs` declares `pub mod leaf;`; because `sub.rs` is not a
    // `mod.rs`/`lib.rs`, the child lives at `src/sub/leaf.rs`, not
    // `src/leaf.rs`. The old `parent_file.parent()` logic dropped it.
    let root = build_crate_tree(&manifest_dir("nested_modules"), "nested_modules").expect("build");
    let sub = root
        .submodules
        .iter()
        .find(|m| m.name == "sub")
        .expect("sub mod");
    let leaf = sub
        .submodules
        .iter()
        .find(|m| m.name == "leaf")
        .expect("sub::leaf should resolve to src/sub/leaf.rs");
    let item_names: Vec<_> = leaf.items.iter().map(|i| i.name.as_str()).collect();
    assert!(item_names.contains(&"in_sub_leaf"), "got {item_names:?}");
    assert_eq!(leaf.canonical.display(), "nested_modules::sub::leaf");
}

#[test]
fn inline_mod_in_file_module_resolves_nested_dir() {
    // `src/sub.rs` has an inline `mod wrap { mod nested; }`. The inline
    // `wrap` owns a deeper dir, so the file child resolves at
    // `src/sub/wrap/nested.rs` — exercising the `mod_dir.join(inline)`
    // threading, not just the file-stem rule.
    let root = build_crate_tree(&manifest_dir("nested_modules"), "nested_modules").expect("build");
    let sub = root.submodules.iter().find(|m| m.name == "sub").unwrap();
    let wrap = sub
        .submodules
        .iter()
        .find(|m| m.name == "wrap")
        .expect("inline wrap mod");
    let nested = wrap
        .submodules
        .iter()
        .find(|m| m.name == "nested")
        .expect("wrap::nested should resolve to src/sub/wrap/nested.rs");
    let item_names: Vec<_> = nested.items.iter().map(|i| i.name.as_str()).collect();
    assert!(item_names.contains(&"in_wrap_nested"), "got {item_names:?}");
    assert_eq!(
        nested.canonical.display(),
        "nested_modules::sub::wrap::nested"
    );
}

#[test]
fn path_attribute_overrides_resolution() {
    let root = build_crate_tree(&manifest_dir("path_attr"), "path_attr").expect("build");
    let renamed = root
        .submodules
        .iter()
        .find(|m| m.name == "renamed")
        .expect("renamed submodule");
    let names: Vec<_> = renamed.items.iter().map(|i| i.name.as_str()).collect();
    assert!(names.contains(&"actually_in_other_file"), "got {names:?}");
}

#[test]
fn inline_mod_becomes_submodule_with_same_file() {
    let root = build_crate_tree(&manifest_dir("inline_mod"), "inline_mod").expect("build");
    let inner = root
        .submodules
        .iter()
        .find(|m| m.name == "inner")
        .expect("inline submodule");
    assert_eq!(inner.file, root.file, "inline mod shares parent file");
    let names: Vec<_> = inner.items.iter().map(|i| i.name.as_str()).collect();
    assert!(names.contains(&"nested_fn"), "got {names:?}");
}

#[test]
fn visibility_is_extracted_per_item() {
    let root = build_crate_tree(&manifest_dir("flat_lib"), "flat_lib").expect("build");
    let pub_fn = root.items.iter().find(|i| i.name == "public_fn").unwrap();
    let priv_struct = root
        .items
        .iter()
        .find(|i| i.name == "PrivateStruct")
        .unwrap();
    let pub_crate_const = root.items.iter().find(|i| i.name == "INTERNAL").unwrap();
    assert_eq!(pub_fn.visibility, Visibility::Public);
    assert_eq!(priv_struct.visibility, Visibility::Private);
    assert_eq!(pub_crate_const.visibility, Visibility::PubCrate);
}

#[test]
fn missing_mod_target_is_recorded_as_broken() {
    // `mod ghost;` with no file should not panic; the resolver records a
    // BrokenModDecl entry on the parent module so consumers can flag
    // the dangling declaration.
    let root = build_crate_tree(&manifest_dir("missing_mod"), "missing_mod").expect("build");
    assert!(root.submodules.iter().all(|m| m.name != "ghost"));
    assert!(
        root.broken_mod_decls.iter().any(|d| d.name == "ghost"),
        "expected `ghost` to be recorded as a broken mod decl, got: {:?}",
        root.broken_mod_decls,
    );
}

// --- signature-exposure walk (signature.rs) ---

/// Every distinct canonical recorded as a signature exposure, regardless of
/// visibility.
fn signature_exposed(root: &Module) -> std::collections::HashSet<String> {
    root.walk()
        .flat_map(|m| m.signature_exposures.iter())
        .map(|e| e.canonical.display().to_string())
        .collect()
}

#[test]
fn signature_exposure_records_public_signature_types() {
    let root =
        build_crate_tree(&manifest_dir("signature_exposure"), "signature_exposure").expect("build");
    // All recorded exposures are Public (the walk only records Public positions).
    assert!(
        root.walk()
            .flat_map(|m| m.signature_exposures.iter())
            .all(|e| e.enclosing_vis == Visibility::Public),
        "every recorded exposure should be Public",
    );
    let exposed = signature_exposed(&root);
    for ty in [
        "signature_exposure::inner::RetType",    // fn return
        "signature_exposure::inner::ParamType",  // fn parameter
        "signature_exposure::inner::AssocType",  // trait-impl associated type (E0446)
        "signature_exposure::inner::FieldType",  // pub field of pub struct
        "signature_exposure::inner::NestedType", // nested in Vec<…>
        "signature_exposure::inner::BareFnArg",  // bare-fn pointer param
        "signature_exposure::inner::BareFnRet",  // bare-fn pointer return
        "signature_exposure::inner::DefaultArg", // generic default type arg
    ] {
        assert!(
            exposed.contains(ty),
            "expected `{ty}` exposed; got {exposed:?}"
        );
    }
}

#[test]
fn signature_exposure_skips_non_public_and_body_positions() {
    let root =
        build_crate_tree(&manifest_dir("signature_exposure"), "signature_exposure").expect("build");
    let exposed = signature_exposed(&root);
    for ty in [
        "signature_exposure::inner::BodyOnly", // referenced only in a fn body
        "signature_exposure::inner::CrateOnlyType", // exposed only by a pub(crate) fn
        "signature_exposure::inner::PrivFieldType", // pub field of a private struct
    ] {
        assert!(
            !exposed.contains(ty),
            "expected `{ty}` NOT exposed; got {exposed:?}"
        );
    }
}

#[test]
fn builder_attr_records_promoted_types() {
    let root =
        build_crate_tree(&manifest_dir("builder_exposure"), "builder_exposure").expect("build");
    // Like the source-signature walk, attribute-promoted types are recorded at
    // Public (the generated `build()` is public).
    assert!(
        root.walk()
            .flat_map(|m| m.signature_exposures.iter())
            .all(|e| e.enclosing_vis == Visibility::Public),
        "every recorded exposure should be Public",
    );
    let exposed = signature_exposed(&root);
    for ty in [
        "builder_exposure::inner::TbErr", // typed_builder build_method(into = …)
        "builder_exposure::inner::TbErrVis", // … reached past a `vis = "…"` sibling
        "builder_exposure::inner::DbErr", // derive_builder build_fn(error = "…")
    ] {
        assert!(
            exposed.contains(ty),
            "expected `{ty}` exposed; got {exposed:?}"
        );
    }
}

#[test]
fn builder_attr_skips_non_promoting_keys() {
    let root =
        build_crate_tree(&manifest_dir("builder_exposure"), "builder_exposure").expect("build");
    let exposed = signature_exposed(&root);
    // Named only by `crate_module_path = …` (not build_method/build_fn), so the
    // targeted scan must not record it — proves we don't broadly scan every
    // `#[builder(…)]` token stream. The bare-`into` and `build_fn(private)`
    // cases in the fixture also exercise the parse without recording anything.
    assert!(
        !exposed.contains("builder_exposure::inner::NotExposed"),
        "expected `inner::NotExposed` NOT exposed; got {exposed:?}"
    );
}

#[test]
fn builder_attr_records_provenance() {
    let root =
        build_crate_tree(&manifest_dir("builder_exposure"), "builder_exposure").expect("build");
    let provenance: Vec<_> = root.walk().flat_map(|m| m.fact_provenance.iter()).collect();

    // The builder plugin tags every promoted exposure with its provenance — the
    // asserting plugin and the specific build_method/build_fn rule.
    assert!(
        provenance
            .iter()
            .any(|p| p.by.plugin == "typed_builder" && p.by.rule == "build_method.into"),
        "expected a typed_builder provenance entry; got {:?}",
        provenance
            .iter()
            .map(|p| (p.by.plugin, p.by.rule))
            .collect::<Vec<_>>(),
    );
    assert!(
        provenance
            .iter()
            .any(|p| p.by.plugin == "derive_builder" && p.by.rule == "build_fn.error"),
        "expected a derive_builder provenance entry",
    );
    // Builder facts are all exposures, each pointing at the promoted type path and
    // anchored at the `#[builder]` attribute span.
    assert!(
        provenance
            .iter()
            .all(|p| p.kind == crate::plugins::FactKind::Exposure && p.by.trigger.is_some()),
        "builder provenance: every entry is an Exposure with a trigger span",
    );
    assert!(
        provenance
            .iter()
            .any(|p| p.path.display().contains("TbErr")),
        "provenance records the promoted type's canonical path",
    );
}

#[test]
fn tier_h_assertions_record_local_fact_references() {
    // A strum derive and a `#[serde(with = "…")]` whose only references to `strum`
    // and the `helpers` module exist through their macro-expansion contracts.
    let src = r#"
        #[derive(EnumString)]
        pub enum Mode { On, Off }

        pub struct Wrapper {
            #[serde(with = "helpers")]
            pub bytes: Vec<u8>,
        }

        pub mod helpers {
            pub fn serialize() {}
            pub fn deserialize() {}
        }
    "#;
    let parent_canonical = ResolvedPath::new(["demo".to_string()]);
    let contents = collect_module_contents(
        &parse_items(src),
        std::path::Path::new("<test>"),
        std::path::Path::new("<test>"),
        &parent_canonical,
        &default_markers(),
        false,
        Vec::new(),
        IncludeCtx::none(),
    )
    .expect("collect");

    let refs: Vec<String> = contents
        .fact_references
        .iter()
        .map(|p| p.display())
        .collect();
    // strum derive ⇒ the `strum` runtime crate; serde-with ⇒ the sibling module's
    // contract helpers, resolved against this module's scope.
    assert!(
        refs.contains(&"strum".to_string()),
        "expected `strum`; got {refs:?}"
    );
    assert!(
        refs.contains(&"demo::helpers::serialize".to_string())
            && refs.contains(&"demo::helpers::deserialize".to_string()),
        "expected serde-with children resolved against the sibling module; got {refs:?}",
    );

    // The whole point of `fact_references`: these edges never enter `occurrences`,
    // so they stay out of the SCIP projection and `Module::references`.
    let occ: Vec<String> = contents
        .occurrences
        .iter()
        .filter_map(|o| o.path.as_ref())
        .map(|p| p.display())
        .collect();
    assert!(
        !occ.iter()
            .any(|p| p == "strum" || p.starts_with("demo::helpers")),
        "asserted refs must not leak into occurrences; got {occ:?}",
    );

    // The provenance side table attributes each fact to its owning crate + rule.
    let prov: Vec<(&str, &str)> = contents
        .fact_provenance
        .iter()
        .map(|p| (p.by.plugin, p.by.rule))
        .collect();
    assert!(prov.contains(&("strum", "strum-derive")), "got {prov:?}");
    assert!(prov.contains(&("serde", "serde-with")), "got {prov:?}");
}

// --- code-path extraction (regular non-macro item bodies) ---

fn parse_items(src: &str) -> Vec<syn::Item> {
    syn::parse_file(src).expect("valid file").items
}

fn default_markers() -> Vec<String> {
    vec!["workspace_syn".into(), "syn_workspace_marker".into()]
}

fn collect_refs(src: &str, crate_name: &str) -> Vec<String> {
    let parent_canonical = ResolvedPath::new([crate_name.to_string()]);
    let items = parse_items(src);
    let markers = default_markers();
    let contents = collect_module_contents(
        &items,
        std::path::Path::new("<test>"),
        std::path::Path::new("<test>"),
        &parent_canonical,
        &markers,
        false,
        Vec::new(),
        IncludeCtx::none(),
    )
    .expect("collect");
    let out: std::collections::BTreeSet<String> = contents
        .occurrences
        .iter()
        .filter(|o| o.origin != Origin::Macro)
        .filter_map(|o| o.path.as_ref())
        .map(|p| p.display())
        .collect();
    out.into_iter().collect()
}

/// Bare-name segments of `Origin::MacroCall` occurrences — the macro
/// invocations the core `MacroCallPass` later binds to same-crate
/// `macro_rules!` definitions.
fn macro_call_names(src: &str) -> Vec<String> {
    let parent_canonical = ResolvedPath::new(["demo".to_string()]);
    let items = parse_items(src);
    let markers = default_markers();
    let contents = collect_module_contents(
        &items,
        std::path::Path::new("<test>"),
        std::path::Path::new("<test>"),
        &parent_canonical,
        &markers,
        false,
        Vec::new(),
        IncludeCtx::none(),
    )
    .expect("collect");
    contents
        .occurrences
        .iter()
        .filter(|o| o.origin == Origin::MacroCall)
        .map(|o| o.segments.join("::"))
        .collect()
}

#[test]
fn bare_macro_invocation_is_captured_as_macrocall() {
    // A bare single-ident macro invocation is captured for the MacroCallPass
    // to bind to a same-crate `macro_rules!`; this is what stops an exported
    // macro used only intra-crate from being flagged `unused-pub`.
    let names = macro_call_names("fn f() { my_macro!(Thing); }");
    assert!(names.contains(&"my_macro".to_string()), "got {names:?}");
}

#[test]
fn multi_segment_macro_invocation_is_not_macrocall() {
    // `m::foo!()` is an ordinary multi-segment Origin::Code run, not a bare
    // MacroCall, and the qualified path is still seen as a code reference.
    let names = macro_call_names("fn f() { serde_json::json!({}); }");
    assert!(names.is_empty(), "got {names:?}");
    let refs = collect_refs("fn f() { serde_json::json!({}); }", "demo");
    assert!(
        refs.contains(&"serde_json::json".to_string()),
        "got {refs:?}"
    );
}

#[test]
fn path_segment_before_bang_is_not_a_macrocall() {
    // `log::debug!(...)`: `log` is a crate/module segment (followed by `::`),
    // not a bare macro name — it must stay a code path to the external crate,
    // never mistaken for a local macro invocation (the increment-6 invariant
    // that a `macro_rules! log` must not shadow the `log` crate).
    let names = macro_call_names("fn f() { log::debug!(\"x\"); }");
    assert!(!names.iter().any(|n| n == "log"), "got {names:?}");
    let refs = collect_refs("fn f() { log::debug!(\"x\"); }", "demo");
    assert!(refs.contains(&"log::debug".to_string()), "got {refs:?}");
}

#[test]
fn code_path_extracts_fully_qualified_external() {
    let refs = collect_refs("fn f() { let _ = std::env::args(); }", "demo");
    assert!(refs.contains(&"std::env::args".to_string()), "got {refs:?}");
}

#[test]
fn code_path_substitutes_use_binding() {
    let refs = collect_refs("use other::Bar; fn f() -> Bar { Bar::new() }", "demo");
    assert!(refs.contains(&"other::Bar".to_string()), "got {refs:?}");
    assert!(
        refs.contains(&"other::Bar::new".to_string()),
        "got {refs:?}"
    );
}

#[test]
fn code_path_substitutes_renamed_use() {
    // `use foo::Bar as Baz; Baz::method()` → canonical foo::Bar::method
    let refs = collect_refs("use foo::Bar as Baz; fn f() { Baz::method(); }", "demo");
    assert!(
        refs.contains(&"foo::Bar::method".to_string()),
        "got {refs:?}"
    );
}

#[test]
fn code_path_resolves_crate_prefix() {
    let refs = collect_refs("fn f() { crate::inner::go(); }", "demo");
    assert!(
        refs.contains(&"demo::inner::go".to_string()),
        "got {refs:?}"
    );
}

#[test]
fn code_path_resolves_sibling_local() {
    let refs = collect_refs(
        "fn helper() {} fn f() { helper(); helper::Sub::go(); }",
        "demo",
    );
    // First segment of `helper::Sub::go` resolves crate-local.
    assert!(
        refs.contains(&"demo::helper::Sub::go".to_string()),
        "got {refs:?}"
    );
    // A *bare* single-ident sibling call (`helper()`) is also recorded — it
    // matches a sibling name, so the keep-filter retains it and resolution
    // anchors it to the current module.
    assert!(refs.contains(&"demo::helper".to_string()), "got {refs:?}");
}

#[test]
fn code_path_skips_own_declaration_ident() {
    // A never-used item's own declaring ident must NOT be recorded as a
    // reference to itself — otherwise `unused-pub` sees a same-crate ref and
    // misclassifies it `IntraCrate` ("pub(crate)") instead of `Unused`.
    let refs = collect_refs("fn lonely() { let _ = 1; }", "demo");
    assert!(
        !refs.contains(&"demo::lonely".to_string()),
        "declaration self-ref recorded; got {refs:?}"
    );
}

#[test]
fn code_path_keeps_recursive_self_call() {
    // The skip is span-based, not name-based: a recursive *call* in the body
    // sits at a different span than the declaration, so it stays a genuine
    // reference (only the declaring ident itself is dropped).
    let refs = collect_refs("fn recur() { recur(); }", "demo");
    assert!(refs.contains(&"demo::recur".to_string()), "got {refs:?}");
}

#[test]
fn code_path_resolves_function_local_module_use() {
    // A `use crate::m::sub;` *inside a fn body*, then `sub::ITEM`: the
    // crate-local const must be seen as referenced (regex-syntax's
    // `age::BY_NAME` FP class — module imported in a fn, member accessed by
    // `Mod::ITEM`). Module-level uses alone wouldn't catch this.
    let refs = collect_refs(
        "mod m { pub mod sub { pub const ITEM: u32 = 0; } } \
         fn f() { use crate::m::sub; let _ = sub::ITEM; }",
        "demo",
    );
    assert!(
        refs.contains(&"demo::m::sub::ITEM".to_string()),
        "function-local module-import use not honored; got {refs:?}"
    );
}

#[test]
fn code_path_resolves_function_local_braced_use() {
    // A function-local braced `use crate::m::{sub::ITEM, other};`, then a
    // bare `ITEM` (regex-automata's `PERL_WORD` FP class — braced group with
    // a shared `crate::m` prefix imported inside a fn).
    let refs = collect_refs(
        "mod m { pub mod sub { pub const ITEM: u32 = 0; } pub fn other() {} } \
         fn f() { use crate::m::{sub::ITEM, other}; let _ = ITEM; other(); }",
        "demo",
    );
    assert!(
        refs.contains(&"demo::m::sub::ITEM".to_string()),
        "function-local braced use not honored; got {refs:?}"
    );
}

/// `segments.join("::")` of every occurrence with the given origin. The Phase B
/// `GlobImportPass` (which binds glob candidates to targets) runs at the
/// workspace level, after `collect_module_contents`, so resolved-path helpers
/// like `collect_refs` can't observe glob binding here — but the capture this
/// pass *depends on* (the `GlobUse` target and the `GlobCandidate` ident) is
/// produced inside `collect_module_contents` and is observable directly.
fn occurrence_segments(src: &str, origin: Origin) -> Vec<String> {
    let parent_canonical = ResolvedPath::new(["demo".to_string()]);
    let items = parse_items(src);
    let markers = default_markers();
    let contents = collect_module_contents(
        &items,
        std::path::Path::new("<test>"),
        std::path::Path::new("<test>"),
        &parent_canonical,
        &markers,
        false,
        Vec::new(),
        IncludeCtx::none(),
    )
    .expect("collect");
    contents
        .occurrences
        .iter()
        .filter(|o| o.origin == origin)
        .map(|o| o.segments.join("::"))
        .collect()
}

#[test]
fn function_local_glob_use_is_recorded() {
    // Regression: a bare ident brought into scope by a *function-local* glob
    // import (`use data::*;` inside a fn body) was invisible to the resolver —
    // the glob emitted no `GlobUse` occurrence and never flipped the
    // `GlobCandidate` capture gate, so `data::AF` read as unused (≈460 false
    // positives in a real auto-generated country table whose consts are reached
    // only via `use data::*; … AF`). Assert both halves the Phase B
    // `GlobImportPass` later needs: the recorded glob target and the captured
    // bare-ident candidate.
    let src = "mod data { pub const AF: &str = \"af\"; } \
               fn f() -> &'static str { use data::*; AF }";
    let glob_uses = occurrence_segments(src, Origin::GlobUse);
    assert!(
        glob_uses.contains(&"demo::data".to_string()),
        "function-local `use data::*;` not recorded as a crate-anchored GlobUse \
         target; got {glob_uses:?}"
    );
    let candidates = occurrence_segments(src, Origin::GlobCandidate);
    assert!(
        candidates.contains(&"AF".to_string()),
        "bare `AF` not captured as a GlobCandidate; got {candidates:?}"
    );
}

#[test]
fn code_path_records_bare_sibling_type_reference() {
    // The thiserror FP class: a sibling type referenced by *bare* name in a
    // field type (`Option<Sib>`) and a struct literal (`Sib { .. }`) must be
    // recorded, so `unused-pub` doesn't think `Sib` is unused.
    let refs = collect_refs(
        "struct Sib; struct Wrap { inner: Option<Sib> } fn mk() -> Sib { Sib }",
        "demo",
    );
    assert!(
        refs.contains(&"demo::Sib".to_string()),
        "bare sibling type ref not recorded; got {refs:?}"
    );
}

#[test]
fn code_path_skips_unmatched_single_ident() {
    let refs = collect_refs("fn f() { let x = 5; let _ = x; }", "demo");
    assert!(
        !refs.iter().any(|r| r == "x"),
        "got {refs:?} — bare locals should not be recorded as references"
    );
}

#[test]
fn code_path_captures_extern_crate() {
    let refs = collect_refs("extern crate foo;", "demo");
    assert!(refs.contains(&"foo".to_string()), "got {refs:?}");
}

#[test]
fn code_path_captures_macro_invocation_path() {
    let refs = collect_refs("fn f() { serde_json::json!({\"a\": 1}); }", "demo");
    assert!(
        refs.contains(&"serde_json::json".to_string()),
        "got {refs:?}"
    );
}

#[test]
fn code_path_skips_use_statements() {
    // Use statements produce use_bindings, not references.
    let refs = collect_refs("use foo::Bar;", "demo");
    // We expect no entries for `foo::Bar` in references — that's a binding.
    assert!(
        !refs.contains(&"foo::Bar".to_string()),
        "got {refs:?} — use statements should not contribute to references"
    );
}

#[test]
fn code_path_skips_macro_rules_definitions() {
    // macro_rules! bodies feed macro_implicit_refs, not references.
    let src = "macro_rules! m { () => { foo::bar() }; }";
    let refs = collect_refs(src, "demo");
    assert!(
        !refs.contains(&"foo::bar".to_string()),
        "got {refs:?} — macro_rules bodies belong in macro_implicit_refs"
    );
}

#[test]
fn code_path_captures_struct_field_types() {
    let refs = collect_refs("use other::Inner; struct S { f: Inner }", "demo");
    assert!(refs.contains(&"other::Inner".to_string()), "got {refs:?}");
}
