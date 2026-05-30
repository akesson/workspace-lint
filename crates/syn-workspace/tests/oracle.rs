//! Committed differential-oracle regression net (ROADMAP Phase 1, pre-Phase-0).
//!
//! For each fixture under `tests/oracle/<name>/`, this parses the committed,
//! normalized oracle JSON in `expected/` — distilled from rustdoc JSON and
//! `rust-analyzer scip` by `tools/oracle-bless` — and diffs it against the LIVE
//! `syn-workspace` resolver. It is the "buildable before Phase 0" net from the
//! roadmap: it runs on the fast path (`serde_json` only, no rust-analyzer or
//! nightly), so a resolver regression in any of four dimensions fails CI:
//!
//!   1. def/visibility (rustdoc) — for this fixture, every item-`pub` def the
//!      resolver enumerates is also externally reachable, so its set must equal
//!      rustdoc's importable-path set; impl-block methods are accounted for in
//!      `known_impl_methods` (the documented enumeration gap).
//!   2. def witness (SCIP) — an independent oracle: every def the resolver
//!      enumerates must also appear in rust-analyzer's definition set.
//!   3. re-export canonicalization — `pub use` chains must resolve to the same
//!      definition rustdoc resolves them to.
//!   4. dependency set — every declared dependency SCIP proves is referenced
//!      must be visible to the resolver, or `unused-deps` would false-positive.
//!
//! Regenerate the committed oracles after changing a fixture or bumping the
//! pinned toolchain:
//!
//!     cargo run --manifest-path tools/oracle-bless/Cargo.toml

use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use syn_workspace::{Crate, DepSection, ItemKind, ResolvedPath, Workspace};

/// rust-analyzer emits UTF-8 code-unit (byte) column offsets; the byte-span
/// alignment this net relies on assumes it. Drift here fails loudly.
const EXPECTED_POSITION_ENCODING: &str = "UTF8CodeUnitOffsetFromLineStart";

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
    let scip = load_json(&base.join("expected/scip.json"));

    check_def_visibility(&ws, "oracle-core", &rustdoc);
    check_scip_def_witness(&ws, "oracle-core", &scip);
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
    let extra: Vec<_> = syn.difference(&oracle).collect();
    assert!(
        extra.is_empty(),
        "REGRESSION: syn-workspace enumerated public defs rustdoc does not (spurious or wrong-path): {extra:?}"
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

/// Check 3 — `pub use` re-exports must canonicalize to the definition rustdoc
/// resolves them to.
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

/// Check 4 — every declared dependency SCIP proves is referenced must be visible
/// to the resolver, otherwise `unused-deps` would emit a false positive.
fn check_dependency_set(ws: &Workspace, krate: &str, scip: &Value) {
    let krate = member(ws, krate);
    let code = krate.code_name();

    let declared: BTreeSet<String> = krate
        .declared_deps()
        .filter(|d| matches!(d.section, DepSection::Dependencies))
        .map(|d| d.normalized_name)
        .collect();
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

fn is_def_kind(k: ItemKind) -> bool {
    matches!(
        k,
        ItemKind::Fn
            | ItemKind::Struct
            | ItemKind::Enum
            | ItemKind::Union
            | ItemKind::Trait
            | ItemKind::TypeAlias
            | ItemKind::Const
            | ItemKind::Static
    )
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
