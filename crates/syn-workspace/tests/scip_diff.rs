//! Occurrence-level SCIP differential harness (see `DESIGN-ir-pipeline.md` §10).
//!
//! Sharpens the set-level dependency oracle in `oracle.rs` (which compares
//! crate-name *sets*) to **occurrence granularity**: it diffs the resolver's
//! SCIP-shaped projection ([`Workspace::scip_occurrences`]) against a committed,
//! normalized projection of a pinned rust-analyzer's `.scip`
//! (`expected/scip-occurrences.json`, distilled by `tools/oracle-bless`), and
//! reports two numbers for the dependency-lint signal — **cross-crate** in-class
//! references:
//!
//!   - **precision** = of the references the resolver emits, how many
//!     rust-analyzer confirms. This is our false-positive rate; the test gates on
//!     **precision == 100 %** (any unconfirmed reference fails CI).
//!   - **in-class recall** = of rust-analyzer's in-class references, how many the
//!     resolver catches. A ratcheting matched-count floor ([`MIN_CROSS_CRATE_MATCHES`]).
//!
//! Global recall against SCIP is capped by design (method calls, field access,
//! inferred-type paths, locals — none of which the resolver produces). We compare
//! only the *in-class* set, filtered identically on both sides. The current
//! misses are all structural and expected: rust-analyzer emits one occurrence per
//! path *segment* (so a bare `oracle_core` / `geometry` module prefix is its own
//! occurrence) while the resolver emits one per *full path*; plus field
//! references it cannot produce. None are false positives.
//!
//! Fast path: `serde_json` only — no `scip`/`protobuf`, no rust-analyzer, no
//! nightly. Regenerate the committed oracle after a fixture or toolchain change:
//!
//!     cargo run --manifest-path tools/oracle-bless/Cargo.toml

use serde_json::Value;
use std::collections::BTreeSet;
use std::path::Path;
use syn_workspace::{ScipOccurrence, ScipRole, Workspace};

mod common;
use common::{EXPECTED_POSITION_ENCODING, fixture_dir, fixture_workspace_present, load_json};

/// Packages excluded from the cross-crate set: the dependency lints reason about
/// declared workspace/registry deps, not the implicit sysroot.
const SYSROOT: &[&str] = &["core", "std", "alloc", "proc_macro"];

/// Cross-crate in-class references the resolver currently matches against
/// rust-analyzer for the `multi_crate` fixture. The recall floor: **ratchet this
/// UP** as resolution improves (a drop fails the test). Lowering it requires a
/// note explaining the regression. Precision is gated separately at 100 %.
const MIN_CROSS_CRATE_MATCHES: usize = 12;

#[test]
fn multi_crate_scip_diff() {
    let base = fixture_dir("multi_crate");
    // Published-crate guard (mirrors oracle.rs): the fixture `workspace/` subtree
    // is excluded from the packaged crate, so skip cleanly if it's absent.
    if !fixture_workspace_present(&base) {
        eprintln!("oracle fixture absent (packaged crate?) — skipping");
        return;
    }
    let ws = Workspace::load(base.join("workspace")).expect("load multi_crate fixture");
    let oracle = load_json(&base.join("expected/scip-occurrences.json"));

    assert_eq!(
        oracle["position_encoding"].as_str(),
        Some(EXPECTED_POSITION_ENCODING),
        "committed SCIP occurrence oracle uses an unexpected position encoding; \
         the byte-column comparison assumes {EXPECTED_POSITION_ENCODING}"
    );

    // ---- rust-analyzer side (committed, in-class filtered at bless time) ------
    let mut ra_refs: BTreeSet<RefKey> = BTreeSet::new();
    let mut ra_def_syms: BTreeSet<String> = BTreeSet::new();
    for o in oracle["occurrences"].as_array().expect("occurrences array") {
        if !o["in_class"].as_bool().unwrap_or(false) {
            continue;
        }
        let symbol = seg_join(&o["symbol"]);
        match o["role"].as_str() {
            Some("definition") => {
                ra_def_syms.insert(symbol);
            }
            Some("reference") => {
                let crate0 = o["symbol"][0].as_str().unwrap_or("");
                if o["cross_crate"].as_bool().unwrap_or(false) && !SYSROOT.contains(&crate0) {
                    ra_refs.insert((
                        symbol,
                        o["file"].as_str().unwrap_or("").to_string(),
                        o["range"][0].as_i64().unwrap_or(-1),
                    ));
                }
            }
            _ => {}
        }
    }

    // ---- resolver side (live projection) --------------------------------------
    let occ = ws.scip_occurrences();
    let mut our_refs: BTreeSet<RefKey> = BTreeSet::new();
    let mut our_def_syms: BTreeSet<String> = BTreeSet::new();
    for o in &occ {
        let symbol = o.symbol.join("::");
        match o.role {
            ScipRole::Definition if o.in_class => {
                our_def_syms.insert(symbol);
            }
            ScipRole::Reference if o.in_class && o.cross_crate => {
                let crate0 = o.symbol.first().map(String::as_str).unwrap_or("");
                if !SYSROOT.contains(&crate0) {
                    our_refs.insert((symbol, norm_path(&o.file), i64::from(o.line)));
                }
            }
            _ => {}
        }
    }

    // ---- precision / in-class recall ------------------------------------------
    let matched: Vec<&RefKey> = our_refs.intersection(&ra_refs).collect();
    let false_positives: Vec<&RefKey> = our_refs.difference(&ra_refs).collect();
    let missed: Vec<&RefKey> = ra_refs.difference(&our_refs).collect();

    let pct = |n: usize, d: usize| {
        if d == 0 {
            100.0
        } else {
            100.0 * n as f64 / d as f64
        }
    };
    eprintln!(
        "cross-crate in-class references: resolver={}, rust-analyzer={}, matched={}",
        our_refs.len(),
        ra_refs.len(),
        matched.len()
    );
    eprintln!(
        "  precision = {:.1}%   in-class recall = {:.1}%",
        pct(matched.len(), our_refs.len()),
        pct(matched.len(), ra_refs.len())
    );
    for (s, f, l) in &false_positives {
        eprintln!(
            "FALSE POSITIVE: {s} at {f}:{} — emitted by resolver, not confirmed by rust-analyzer",
            l + 1
        );
    }
    for (s, f, l) in &missed {
        eprintln!("MISSED (in-class): {s} at {f}:{}", l + 1);
    }

    // Precision == 100 %: a hard gate. Any reference the resolver emits that
    // rust-analyzer's in-class set doesn't confirm is a false positive.
    assert!(
        false_positives.is_empty(),
        "SCIP precision regression: {} cross-crate reference(s) the resolver emits \
         are not in rust-analyzer's in-class set (false positives) — listed above",
        false_positives.len()
    );

    // In-class recall: a ratcheting floor.
    assert!(
        matched.len() >= MIN_CROSS_CRATE_MATCHES,
        "in-class recall dropped: matched {} < floor {MIN_CROSS_CRATE_MATCHES}; \
         if this is an intentional change, update MIN_CROSS_CRATE_MATCHES with a note",
        matched.len()
    );

    // Secondary witness: every in-class def the resolver enumerates must also be
    // an in-class def for rust-analyzer (symbol-only — def spans aren't compared).
    let missing_defs: Vec<&String> = our_def_syms.difference(&ra_def_syms).collect();
    assert!(
        missing_defs.is_empty(),
        "resolver enumerates definitions absent from rust-analyzer's in-class def set: {missing_defs:?}"
    );

    // Non-ASCII range guard: `café` is 5 UTF-8 bytes / 4 chars. The byte-column
    // conversion must agree with rust-analyzer here, or the encoding drifted.
    check_cafe_range(&occ, &oracle);
}

/// `(symbol, crate-relative file, 0-based start line)` — the occurrence identity
/// used for matching. Column is intentionally excluded: the resolver spans the
/// *first* path segment while rust-analyzer spans the *leaf*, so they share a
/// line but not a column for multi-segment paths.
type RefKey = (String, String, i64);

/// Cross-check the byte-column encoding on the non-ASCII `café` reference: the
/// resolver's derived byte columns must equal rust-analyzer's, and the width must
/// be 5 (UTF-8 bytes), not 4 (chars).
fn check_cafe_range(occ: &[ScipOccurrence], oracle: &Value) {
    let ra = oracle["occurrences"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| {
            seg_join(&o["symbol"]) == "oracle_core::café"
                && o["role"].as_str() == Some("reference")
                && o["range"][0].as_i64() == Some(4)
        })
        .expect("rust-analyzer café reference at line index 4 (the `use` line)");
    let (ra_sc, ra_ec) = (
        ra["range"][1].as_i64().unwrap(),
        ra["range"][2].as_i64().unwrap(),
    );
    assert_eq!(ra_ec - ra_sc, 5, "café should be 5 UTF-8 bytes wide");

    let ours = occ
        .iter()
        .find(|o| {
            o.symbol == ["oracle_core", "café"] && o.role == ScipRole::Reference && o.line == 4
        })
        .expect("resolver café reference at line index 4 (the `use` line)");
    assert_eq!(
        (i64::from(ours.start_col), i64::from(ours.end_col)),
        (ra_sc, ra_ec),
        "café byte-column encoding must match rust-analyzer (non-ASCII regression guard)"
    );
}

fn seg_join(v: &Value) -> String {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("::")
        })
        .unwrap_or_default()
}

/// Crate-relative path as a forward-slash string (committed oracle uses `/`;
/// `PathBuf` renders `\` on Windows).
fn norm_path(p: &Path) -> String {
    p.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}
