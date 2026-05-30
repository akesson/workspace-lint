//! Committed differential-oracle regression net (ROADMAP Phase 1, pre-Phase-0).
//!
//! For each fixture under `tests/oracle/<name>/`, this parses the committed,
//! normalized oracle JSON in `expected/` — distilled from rustdoc JSON and
//! `rust-analyzer scip` by `tools/oracle-bless` — and diffs it against the LIVE
//! `syn-workspace` resolver. It is the "buildable before Phase 0" net from the
//! roadmap: it runs on the fast path (`serde_json` only, no rust-analyzer or
//! nightly), so a resolver regression in any of five dimensions fails CI:
//!
//!   1. def/visibility (rustdoc) — for this fixture, every item-`pub` def the
//!      resolver enumerates is also externally reachable, so its set must equal
//!      rustdoc's importable-path set; impl-block methods are accounted for in
//!      `known_impl_methods` (the documented enumeration gap).
//!   2. def witness (SCIP) — an independent oracle: every def the resolver
//!      enumerates must also appear in rust-analyzer's definition set.
//!   3. module tree + visibility — the FULL tree (incl. private / `pub(crate)`
//!      items, via `--document-private-items`) must match, with visibility tiers
//!      (public / crate / internal) agreeing.
//!   4. re-export canonicalization — `pub use` chains (incl. `as` renames) must
//!      resolve to the same definition rustdoc resolves them to.
//!   5. dependency set — every declared dependency SCIP proves is referenced
//!      must be visible to the resolver, or `unused-deps` would false-positive;
//!      dev/build deps are excluded via the `DepSection` filter.
//!
//! Regenerate the committed oracles after changing a fixture or bumping the
//! pinned toolchain:
//!
//!     cargo run --manifest-path tools/oracle-bless/Cargo.toml

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use syn_workspace::{Crate, DepSection, ItemKind, ResolvedPath, Visibility, Workspace};

/// rust-analyzer emits UTF-8 code-unit (byte) column offsets; the byte-span
/// alignment this net relies on assumes it. Drift here fails loudly.
const EXPECTED_POSITION_ENCODING: &str = "UTF8CodeUnitOffsetFromLineStart";

/// rustdoc JSON schema version the committed oracles were distilled against —
/// the fast-path twin of `oracle-bless`'s `EXPECTED_RUSTDOC_FORMAT`. Because the
/// bless tool isn't run in CI (it needs nightly + rust-analyzer), a stale or
/// newer-schema oracle could otherwise slip in unnoticed; asserting the
/// committed `format_version` here makes any re-bless under a changed schema
/// fail on the fast path until the distiller's field-lookups are re-validated.
const EXPECTED_RUSTDOC_FORMAT: u64 = 57;

#[test]
fn multi_crate() {
    let base = fixture_dir("multi_crate");
    // Published-crate guard: the fixture `workspace/` subtree is excluded from
    // the packaged crate (it has its own [workspace] table), so skip cleanly if
    // run from a package rather than the source tree.
    if !base.join("workspace").exists() {
        eprintln!("oracle fixture absent (packaged crate?) — skipping");
        return;
    }
    let ws = Workspace::load(base.join("workspace")).expect("load multi_crate fixture");
    let rustdoc = load_json(&base.join("expected/rustdoc.json"));
    let rustdoc_private = load_json(&base.join("expected/rustdoc-private.json"));
    let scip = load_json(&base.join("expected/scip.json"));

    // Provenance guard (see EXPECTED_RUSTDOC_FORMAT): both committed rustdoc
    // oracles must carry the schema version the distiller was written against, so
    // a re-bless under a newer nightly can't quietly change what these checks
    // compare to.
    for oracle in [&rustdoc, &rustdoc_private] {
        assert_eq!(
            oracle["format_version"].as_u64(),
            Some(EXPECTED_RUSTDOC_FORMAT),
            "committed rustdoc oracle schema drifted; re-validate tools/oracle-bless, then bump EXPECTED_RUSTDOC_FORMAT"
        );
    }

    check_def_visibility(&ws, "oracle-core", &rustdoc);
    check_scip_def_witness(&ws, "oracle-core", &scip);
    check_module_tree_visibility(&ws, "oracle-core", &rustdoc_private);
    check_reexports(&ws, "oracle_core", &rustdoc);
    check_dependency_set(&ws, "oracle-app", &scip);
}

/// Check 1 — for this fixture every item-`pub` def is also externally reachable,
/// so the resolver's def set must equal rustdoc's importable-path set exactly;
/// impl-block methods are accounted for as a known divergence.
fn check_def_visibility(ws: &Workspace, krate: &str, rustdoc: &Value) {
    let krate = member(ws, krate);
    let syn = syn_def_segments(krate);
    let oracle: BTreeSet<Vec<String>> = json_seg_set(&rustdoc["public_defs"]);

    let missing: Vec<_> = oracle.difference(&syn).collect();
    assert!(
        missing.is_empty(),
        "REGRESSION: syn-workspace stopped enumerating public defs the compiler reports: {missing:?}"
    );
    // Exact equality (not just `syn ⊇ rustdoc`) holds only because every `pub`
    // item in *this* fixture is externally reachable. A maintainer who adds a
    // `pub` item inside a private, non-re-exported module will trip this: the
    // resolver enumerates it syntactically but rustdoc omits it (unreachable),
    // so the path lands in `extra`. That's fixture drift, not a resolver bug —
    // re-export the item or move it out of the externally-reachable set.
    let extra: Vec<_> = syn.difference(&oracle).collect();
    assert!(
        extra.is_empty(),
        "syn-workspace enumerated public defs rustdoc does not — a resolver regression (spurious / wrong-path def) OR fixture drift (a new `pub` item that isn't externally reachable, which rustdoc omits): {extra:?}"
    );

    // The documented enumeration gap must stay a gap, not silently start firing.
    let impl_methods = str_set(&rustdoc["known_impl_methods"]);
    let syn_names: BTreeSet<String> = krate
        .pub_items()
        .filter(|i| is_def_kind(i.kind))
        .map(|i| i.name.clone())
        .collect();
    for m in &impl_methods {
        assert!(
            !syn_names.contains(m),
            "known impl-block method `{m}` is now enumerated by syn — promote it (update the bless fixture / known_impl_methods)"
        );
    }
}

/// Check 2 — independent SCIP witness: every def the resolver enumerates must
/// also be a definition rust-analyzer reports (and the byte-span encoding the
/// net assumes must hold).
fn check_scip_def_witness(ws: &Workspace, krate: &str, scip: &Value) {
    assert_eq!(
        scip["position_encoding"].as_str(),
        Some(EXPECTED_POSITION_ENCODING),
        "REGRESSION: SCIP range encoding changed; the byte-span assumption (café guard) no longer holds"
    );
    let syn = syn_def_segments(member(ws, krate));
    let scip_defs: BTreeSet<Vec<String>> = scip["definitions"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .map(seg_vec)
        .collect();
    let unconfirmed: Vec<_> = syn.difference(&scip_defs).collect();
    assert!(
        unconfirmed.is_empty(),
        "REGRESSION: rust-analyzer does not confirm these resolver defs as definitions: {unconfirmed:?}"
    );
}

/// Check 4 — `pub use` re-exports (incl. `as` renames) must canonicalize to the
/// definition rustdoc resolves them to.
fn check_reexports(ws: &Workspace, code_name: &str, rustdoc: &Value) {
    let reexports = rustdoc["reexports"]
        .as_array()
        .expect("oracle missing `reexports`");
    assert!(
        !reexports.is_empty(),
        "fixture invariant: at least one re-export must be exercised (else this check is vacuous)"
    );
    for re in reexports {
        let name = re["name"].as_str().expect("reexport name");
        let canonical: Vec<String> = seg_vec(&re["canonical"]);
        let importable = ResolvedPath::new([code_name, name]);
        let resolved = ws.resolve_canonical(&importable);
        assert_eq!(
            resolved.segments(),
            canonical.as_slice(),
            "REGRESSION: re-export `{code_name}::{name}` canonicalization changed"
        );
    }
}

/// Check 5 — every declared dependency SCIP proves is referenced must be visible
/// to the resolver, otherwise `unused-deps` would emit a false positive. Also
/// guards that the `DepSection` filter excludes dev/build dependencies.
fn check_dependency_set(ws: &Workspace, krate: &str, scip: &Value) {
    let krate = member(ws, krate);
    let code = krate.code_name();

    let declared: BTreeSet<String> = krate
        .declared_deps()
        .filter(|d| matches!(d.section, DepSection::Dependencies))
        .map(|d| d.normalized_name)
        .collect();

    // The `Dependencies` filter must scope out dev/build deps: `oracle-extra` is
    // a genuinely-used DEV dependency, so it must be classified `DevDependencies`
    // and stay out of the `[dependencies]` set this check compares against.
    let dev: BTreeSet<String> = krate
        .declared_deps()
        .filter(|d| matches!(d.section, DepSection::DevDependencies))
        .map(|d| d.normalized_name)
        .collect();
    assert!(
        dev.contains("oracle_extra"),
        "fixture invariant: oracle-app should declare oracle-extra as a dev-dependency"
    );
    assert!(
        !declared.contains("oracle_extra"),
        "DepSection filter regression: the dev-dependency oracle-extra leaked into the [dependencies] set"
    );

    let scip_pkgs = str_set(&scip["referenced_packages"][&code]);
    let proven_used: BTreeSet<&String> = declared.intersection(&scip_pkgs).collect();
    assert!(
        !proven_used.is_empty(),
        "fixture invariant broken: SCIP proves no declared dependency of `{code}` is referenced"
    );

    let syn_refs: BTreeSet<String> = ws
        .references_from_crate(krate)
        .into_iter()
        .flatten()
        .filter_map(|p| p.crate_name().map(str::to_string))
        .collect();
    for dep in proven_used {
        assert!(
            syn_refs.contains(dep),
            "unused-deps FALSE-POSITIVE risk: SCIP proves `{code}` references `{dep}`, but it is absent from references_from_crate()"
        );
    }

    // The resolver also records the dev-dependency reference (oracle-extra, used
    // from `#[cfg(test)]`), even though the DepSection filter correctly excludes
    // it from the unused-deps comparison set above.
    assert!(
        syn_refs.contains("oracle_extra"),
        "resolver should record the cfg(test) dev-dependency reference oracle-extra"
    );
}

/// Check 3 (broaden) — the FULL module tree. Every module-level def the
/// `--document-private-items` oracle reports (any visibility) must be enumerated
/// by the resolver at the same canonical path, and visibility tiers
/// (public / crate / internal) must agree. Validates private modules + visibility
/// resolution the public-surface check can't see. Private vs `pub(in …)` is
/// intentionally collapsed into "internal" — rustdoc renders both as `restricted`,
/// so the oracle can't distinguish them either.
fn check_module_tree_visibility(ws: &Workspace, krate: &str, private: &Value) {
    let krate = member(ws, krate);
    // Collect into Vecs first so a duplicate canonical segment path — a value/type
    // namespace collision the segments-only key can't represent — fails loudly
    // instead of silently collapsing. The generator keys on (segments, kind, vis);
    // if a future fixture introduces such a collision, add `kind` to the keys here.
    let syn_list: Vec<(Vec<String>, Visibility)> = krate
        .items()
        .filter(|i| is_def_kind(i.kind))
        .map(|i| (i.canonical.segments().to_vec(), i.visibility))
        .collect();
    let oracle_list: Vec<(Vec<String>, String)> = private["module_defs"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .map(|d| {
            (
                seg_vec(&d["segments"]),
                d["visibility"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    let syn: BTreeMap<Vec<String>, Visibility> = syn_list.iter().cloned().collect();
    let oracle: BTreeMap<Vec<String>, String> = oracle_list.iter().cloned().collect();
    assert_eq!(
        syn.len(),
        syn_list.len(),
        "duplicate canonical segment path in resolver items — namespace collision; key by (segments, kind)"
    );
    assert_eq!(
        oracle.len(),
        oracle_list.len(),
        "duplicate canonical segment path in rustdoc-private module_defs — namespace collision; key by (segments, kind)"
    );
    assert!(
        !oracle.is_empty(),
        "fixture invariant: module_defs must be non-empty (else this check is vacuous)"
    );

    let syn_segs: BTreeSet<&Vec<String>> = syn.keys().collect();
    let oracle_segs: BTreeSet<&Vec<String>> = oracle.keys().collect();
    let missing: Vec<_> = oracle_segs.difference(&syn_segs).collect();
    assert!(
        missing.is_empty(),
        "REGRESSION: resolver missing module-level items rustdoc reports (private tree): {missing:?}"
    );
    let extra: Vec<_> = syn_segs.difference(&oracle_segs).collect();
    assert!(
        extra.is_empty(),
        "REGRESSION: resolver enumerated module-level items rustdoc does not: {extra:?}"
    );
    // `syn[segs]` cannot panic: the `missing` assert above guarantees every oracle
    // key is also present in `syn`.
    for (segs, rd_vis) in &oracle {
        assert_eq!(
            vis_tier_syn(syn[segs]),
            vis_tier_rustdoc(rd_vis),
            "REGRESSION: visibility tier mismatch for {segs:?}: syn {:?} vs rustdoc {rd_vis:?}",
            syn[segs]
        );
    }
}

/// Collapse a syn visibility to the externally-observable reachability tier the
/// rustdoc oracle can witness.
///
/// Scoped to public / crate / private — where syntactic and effective visibility
/// coincide. `PubSuper`/`PubIn` map to "internal" syntactically but the fixture
/// deliberately omits them: syn-workspace reports *syntactic* visibility whereas
/// rustdoc reports *effective* visibility, and the two legitimately diverge for
/// `pub(super)`/`pub(in …)` (e.g. `pub(super)` in a crate-root module is
/// effectively crate-visible, which rustdoc renders as `"crate"`, not
/// `"restricted"`). Validating those would need an effective-visibility model the
/// resolver intentionally doesn't build.
fn vis_tier_syn(v: Visibility) -> &'static str {
    match v {
        Visibility::Public => "public",
        Visibility::PubCrate => "crate",
        Visibility::PubSuper | Visibility::PubIn | Visibility::Private => "internal",
    }
}

/// Same tiering for rustdoc's visibility strings. Captured def-kinds surface as
/// `"public"`, `"crate"`, or a `restricted` object collapsed to `"restricted"`
/// (covering bare-private and `pub(in …)`/`pub(super)`). rustdoc's `"default"`
/// string is only emitted for enum variants, which `RD_DEF_KINDS` filters out;
/// it stays in the catch-all for safety.
fn vis_tier_rustdoc(s: &str) -> &'static str {
    match s {
        "public" => "public",
        "crate" => "crate",
        _ => "internal",
    }
}

// --- helpers ---------------------------------------------------------------

fn syn_def_segments(krate: &Crate) -> BTreeSet<Vec<String>> {
    krate
        .pub_items()
        .filter(|i| is_def_kind(i.kind))
        .map(|i| i.canonical.segments().to_vec())
        .collect()
}

fn fixture_dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/oracle")
        .join(name)
}

fn load_json(p: &Path) -> Value {
    let bytes = std::fs::read(p).unwrap_or_else(|e| {
        panic!("read oracle artifact {} ({e}); regenerate with `cargo run --manifest-path tools/oracle-bless/Cargo.toml`", p.display())
    });
    serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("parse {}: {e}", p.display()))
}

fn member<'a>(ws: &'a Workspace, name: &str) -> &'a Crate {
    ws.members()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("fixture member `{name}` not found"))
}

/// Item kinds the rustdoc/SCIP oracles treat as module-level definitions.
///
/// Delegates to [`syn_workspace::is_definition_kind`] — the single source of
/// truth shared with the SCIP emitter — so this net and `scip_diff.rs` can't
/// drift in what counts as a def. It intentionally diverges from the crate's own
/// [`ItemKind::is_definition`], which counts `Macro`: rustdoc's def-kind set has
/// no `macro` entry and these fixtures declare none. The parallel rustdoc-string
/// list `RD_DEF_KINDS` in `tools/oracle-bless/src/main.rs` (the generator side of
/// the same classification) must stay in lockstep; revisit all three if a
/// macro-bearing fixture ever lands.
fn is_def_kind(k: ItemKind) -> bool {
    syn_workspace::is_definition_kind(k)
}

fn seg_vec(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|s| s.as_str().map(str::to_string))
        .collect()
}

fn json_seg_set(v: &Value) -> BTreeSet<Vec<String>> {
    v.as_array()
        .unwrap_or(&vec![])
        .iter()
        .map(|d| seg_vec(&d["segments"]))
        .collect()
}

fn str_set(v: &Value) -> BTreeSet<String> {
    v.as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|s| s.as_str().map(str::to_string))
        .collect()
}
