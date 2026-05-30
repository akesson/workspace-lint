//! oracle-bless — regenerates the committed differential-oracle artifacts for
//! `crates/syn-workspace/tests/oracle/<fixture>/expected/`.
//!
//! It shells out to nightly rustdoc (JSON) and `rust-analyzer scip`, then
//! distills both into small, deterministic, path-relative JSON oracles that the
//! fast test (`crates/syn-workspace/tests/oracle.rs`) parses with serde_json
//! only — keeping `scip`/`protobuf` and the nightly/RA toolchain off the common
//! test path. Run it whenever the fixture or the pinned toolchain changes:
//!
//!     cargo run --manifest-path tools/oracle-bless/Cargo.toml
//!
//! Requires: a `nightly` toolchain (rustdoc JSON) and `rust-analyzer` on PATH.
//!
//! The tool fails loudly rather than emitting a degraded oracle: it pins the
//! rustdoc `format_version` it understands and asserts the distilled output is
//! non-empty, so toolchain drift surfaces as a clear error instead of a silently
//! hollow regression net.

use anyhow::{bail, ensure, Context, Result};
use protobuf::Message;
use scip::types::Index;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

/// rustdoc JSON schema version this distiller is written against. Bumping the
/// nightly toolchain may change this; the tool bails so a human re-validates the
/// field lookups in `rustdoc_oracle` before re-blessing. Recorded in ROADMAP §B.
const EXPECTED_RUSTDOC_FORMAT: u64 = 57;

/// Fixtures to (re)bless. Add new fixture dirs here.
const FIXTURES: &[Fixture] = &[Fixture {
    dir: "multi_crate",
    rustdoc_crate: "oracle-core",
}];

struct Fixture {
    /// Directory under crates/syn-workspace/tests/oracle/
    dir: &'static str,
    /// Crate whose public surface the rustdoc oracle captures (cargo name).
    rustdoc_crate: &'static str,
}

fn main() -> Result<()> {
    let repo = repo_root();
    for fx in FIXTURES {
        bless(&repo, fx).with_context(|| format!("bless fixture `{}`", fx.dir))?;
    }
    println!("\nblessed {} fixture(s).", FIXTURES.len());
    Ok(())
}

fn bless(repo: &Path, fx: &Fixture) -> Result<()> {
    let base = repo.join("crates/syn-workspace/tests/oracle").join(fx.dir);
    let ws = base.join("workspace");
    let expected = base.join("expected");
    std::fs::create_dir_all(&expected)?;
    println!("== fixture {} ==", fx.dir);

    // ---- rustdoc oracle (public def/visibility + re-exports) -------------
    let rd_json = gen_rustdoc_json(&ws, fx.rustdoc_crate, false)?;
    let rd: Value =
        serde_json::from_slice(&std::fs::read(&rd_json)?).context("parse rustdoc JSON")?;
    let rd_oracle = rustdoc_oracle(&rd)?;
    ensure!(
        rd_oracle["public_defs"].as_array().is_some_and(|a| !a.is_empty()),
        "distilled rustdoc oracle has no public_defs — likely a rustdoc schema change the parser silently missed"
    );
    let rd_out = expected.join("rustdoc.json");
    write_json(&rd_out, &rd_oracle)?;
    println!(
        "  wrote {} (rustdoc format_version {EXPECTED_RUSTDOC_FORMAT})",
        rel(repo, &rd_out)
    );

    // ---- private module-tree + visibility oracle (--document-private-items) ----
    // The public oracle above is already distilled into an owned Value before this
    // call wipes target/doc and regenerates it with private items — keep that
    // read-before-overwrite ordering if refactoring.
    let rd_priv_json = gen_rustdoc_json(&ws, fx.rustdoc_crate, true)?;
    let rd_priv: Value = serde_json::from_slice(&std::fs::read(&rd_priv_json)?)
        .context("parse private rustdoc JSON")?;
    let rd_priv_oracle = rustdoc_private_oracle(&rd_priv)?;
    ensure!(
        rd_priv_oracle["module_defs"]
            .as_array()
            .is_some_and(|a| !a.is_empty()),
        "distilled private rustdoc oracle has no module_defs"
    );
    let rd_priv_out = expected.join("rustdoc-private.json");
    write_json(&rd_priv_out, &rd_priv_oracle)?;
    println!("  wrote {}", rel(repo, &rd_priv_out));

    // ---- SCIP oracle (referenced packages per member + definitions) ------
    let scip_path = gen_scip(&ws)?;
    let index = Index::parse_from_bytes(&std::fs::read(&scip_path)?).context("parse SCIP")?;
    ensure!(!index.documents.is_empty(), "rust-analyzer scip produced no documents — did it fail to load the fixture as a workspace?");
    let scip_oracle = scip_oracle(&index)?;
    let scip_out = expected.join("scip.json");
    write_json(&scip_out, &scip_oracle)?;
    println!("  wrote {}", rel(repo, &scip_out));
    Ok(())
}

// ---------------------------------------------------------------------------
// rustdoc → normalized oracle
// ---------------------------------------------------------------------------

/// rustdoc `inner` kind strings counted as module-level definitions. This is the
/// generator-side twin of `is_def_kind` in
/// `crates/syn-workspace/tests/oracle.rs` (the `ItemKind` side of the same
/// classification) — keep the two lists in lockstep. Deliberately omits `macro`:
/// the fixtures declare none and the syn side excludes it too.
const RD_DEF_KINDS: &[&str] = &[
    "function",
    "struct",
    "enum",
    "union",
    "trait",
    "type_alias",
    "constant",
    "static",
];

fn rustdoc_oracle(rd: &Value) -> Result<Value> {
    let fmt = rd["format_version"].as_u64().unwrap_or(0);
    ensure!(
        fmt == EXPECTED_RUSTDOC_FORMAT,
        "rustdoc format_version {fmt} != expected {EXPECTED_RUSTDOC_FORMAT}; re-validate rustdoc_oracle's field lookups, then bump EXPECTED_RUSTDOC_FORMAT"
    );
    let index = rd["index"].as_object().context("rustdoc .index")?;
    let paths = rd["paths"].as_object().context("rustdoc .paths")?;

    // (segments, kind, name) — a TOTAL key so type/value-namespace collisions
    // (same canonical path) never reorder across toolchain id reassignments.
    let mut public_defs: BTreeSet<(Vec<String>, String, String)> = BTreeSet::new();
    let mut impl_methods: BTreeSet<String> = BTreeSet::new();
    let mut reexports: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for (id, item) in index {
        if item["crate_id"].as_i64() != Some(0) {
            continue;
        }
        if let Some(u) = item["inner"].get("use") {
            if item["visibility"].as_str() == Some("public") {
                if let (Some(name), Some(canon)) = (
                    u["name"].as_str(),
                    u["id"].as_i64().and_then(|tid| path_segments(paths, tid)),
                ) {
                    reexports.insert(name.to_string(), canon);
                }
            }
            continue;
        }
        let Some(name) = item["name"].as_str() else {
            continue;
        };
        if item["visibility"].as_str() != Some("public") {
            continue;
        }
        let kind = item["inner"]
            .as_object()
            .and_then(|o| o.keys().next())
            .map(String::as_str)
            .unwrap_or("");
        if !RD_DEF_KINDS.contains(&kind) {
            continue;
        }
        // In `paths` ⇒ importable module-level def. Absent ⇒ impl/nested method
        // (the documented syn enumeration gap).
        match paths.get(id) {
            Some(p) => {
                let segs = p["path"].as_array().map(|a| seg_vec(a)).unwrap_or_default();
                public_defs.insert((segs, kind.to_string(), name.to_string()));
            }
            None => {
                impl_methods.insert(name.to_string());
            }
        }
    }

    let public_defs: Vec<Value> = public_defs
        .into_iter()
        .map(|(segments, kind, name)| json!({ "name": name, "kind": kind, "segments": segments }))
        .collect();
    let reexports: Vec<Value> = reexports
        .into_iter()
        .map(|(name, canonical)| json!({ "name": name, "canonical": canonical }))
        .collect();

    Ok(json!({
        "_doc": "Generated by tools/oracle-bless; pinned rustdoc format_version. Do not edit by hand.",
        "format_version": fmt,
        "public_defs": public_defs,
        "known_impl_methods": impl_methods.into_iter().collect::<Vec<_>>(),
        "reexports": reexports,
    }))
}

fn path_segments(paths: &Map<String, Value>, id: i64) -> Option<Vec<String>> {
    paths.get(&id.to_string())?["path"]
        .as_array()
        .map(|a| seg_vec(a))
}

/// Like `rustdoc_oracle` but over `--document-private-items` output: every
/// crate-local module-level def-kind item with its visibility — for validating
/// the full module tree + visibility resolution, not just the public surface.
fn rustdoc_private_oracle(rd: &Value) -> Result<Value> {
    let fmt = rd["format_version"].as_u64().unwrap_or(0);
    ensure!(
        fmt == EXPECTED_RUSTDOC_FORMAT,
        "private rustdoc format_version {fmt} != expected {EXPECTED_RUSTDOC_FORMAT}"
    );
    let index = rd["index"].as_object().context("rustdoc .index")?;
    let paths = rd["paths"].as_object().context("rustdoc .paths")?;

    // (segments, kind, visibility) — a TOTAL key for deterministic ordering.
    let mut defs: BTreeSet<(Vec<String>, String, String)> = BTreeSet::new();
    for (id, item) in index {
        if item["crate_id"].as_i64() != Some(0) {
            continue;
        }
        if item["name"].as_str().is_none() {
            continue;
        }
        let kind = item["inner"]
            .as_object()
            .and_then(|o| o.keys().next())
            .map(String::as_str)
            .unwrap_or("");
        if !RD_DEF_KINDS.contains(&kind) {
            continue;
        }
        // Module-level only (present in `paths`); impl/nested methods are out of
        // scope, just as in the public oracle. NB: a *module-level* def-kind item
        // unexpectedly absent from `paths` would also drop here silently and then
        // surface downstream as a misleading "resolver enumerated items rustdoc
        // does not" test failure — none exist for the current fixtures.
        let Some(p) = paths.get(id) else {
            continue;
        };
        let segs = p["path"].as_array().map(|a| seg_vec(a)).unwrap_or_default();
        defs.insert((
            segs,
            kind.to_string(),
            visibility_string(&item["visibility"]),
        ));
    }

    let module_defs: Vec<Value> = defs
        .into_iter()
        .map(|(segments, kind, visibility)| {
            json!({ "segments": segments, "kind": kind, "visibility": visibility })
        })
        .collect();

    Ok(json!({
        "_doc": "Generated by tools/oracle-bless (--document-private-items). Do not edit by hand.",
        "format_version": fmt,
        "module_defs": module_defs,
    }))
}

/// rustdoc visibility for a captured def-kind item is the string `"public"` or
/// `"crate"`, or a `restricted` OBJECT (`{ "restricted": { parent, path } }`) for
/// bare-private / `pub(super)` / `pub(in …)` items — collapsed here to
/// `"restricted"`, which also drops the object's unstable numeric `parent` id (so
/// the artifact stays deterministic). rustdoc's `"default"` string is only emitted
/// for enum variants, which never reach this (not in `RD_DEF_KINDS`).
fn visibility_string(v: &Value) -> String {
    v.as_str()
        .map(str::to_string)
        .unwrap_or_else(|| "restricted".into())
}

fn seg_vec(a: &[Value]) -> Vec<String> {
    a.iter()
        .filter_map(|s| s.as_str().map(str::to_string))
        .collect()
}

// ---------------------------------------------------------------------------
// SCIP → normalized oracle
// ---------------------------------------------------------------------------

fn scip_oracle(index: &Index) -> Result<Value> {
    // RA sets a single index-wide encoding; assert documents agree rather than
    // trusting `first()`.
    let enc = index
        .documents
        .first()
        .map(|d| format!("{:?}", d.position_encoding.enum_value_or_default()))
        .unwrap_or_default();
    for d in &index.documents {
        let e = format!("{:?}", d.position_encoding.enum_value_or_default());
        ensure!(
            e == enc,
            "SCIP documents disagree on position_encoding ({enc} vs {e})"
        );
    }

    let mut referenced: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut definitions: BTreeSet<Vec<String>> = BTreeSet::new();
    let def_role = scip::types::SymbolRole::Definition as i32;

    for doc in &index.documents {
        let member = member_code_name(&doc.relative_path).with_context(|| {
            format!(
                "SCIP document path `{}` has no `crates/<name>/` segment",
                doc.relative_path
            )
        })?;
        let bucket = referenced.entry(member).or_default();
        for occ in &doc.occurrences {
            if let Some((pkg, segs)) = parse_symbol(&occ.symbol) {
                bucket.insert(pkg);
                if occ.symbol_roles & def_role != 0 {
                    definitions.insert(segs);
                }
            }
        }
    }

    let referenced_packages: Map<String, Value> = referenced
        .into_iter()
        .map(|(k, v)| (k, json!(v.into_iter().collect::<Vec<_>>())))
        .collect();

    Ok(json!({
        "_doc": "Generated by tools/oracle-bless. Do not edit by hand.",
        "position_encoding": enc,
        "referenced_packages": referenced_packages,
        "definitions": definitions.into_iter().collect::<Vec<_>>(),
    }))
}

/// `crates/oracle-app/src/lib.rs` → `oracle_app` (code name). `None` if the path
/// has no `crates/<name>/` segment (an unexpected layout we want to surface).
fn member_code_name(relpath: &str) -> Option<String> {
    let parts: Vec<&str> = relpath.split('/').collect();
    let i = parts.iter().position(|p| *p == "crates")?;
    parts.get(i + 1).map(|d| d.replace('-', "_"))
}

/// Parse a SCIP symbol into `(package_code_name, [package, ..descriptors])` via
/// the single canonical parser (`scip::symbol::parse_symbol`, which un-escapes
/// embedded spaces and backtick-wrapped non-ASCII idents). Cargo package name is
/// normalized `-`→`_` to match syn-workspace's code-name form. `None` for local
/// symbols (no package).
fn parse_symbol(symbol: &str) -> Option<(String, Vec<String>)> {
    let sym = scip::symbol::parse_symbol(symbol).ok()?;
    let pkg = sym.package.as_ref()?.name.replace('-', "_");
    if pkg.is_empty() {
        return None;
    }
    let mut segs = vec![pkg.clone()];
    segs.extend(
        sym.descriptors
            .iter()
            .map(|d| d.name.clone())
            .filter(|n| !n.is_empty()),
    );
    Some((pkg, segs))
}

// ---------------------------------------------------------------------------
// generation + io
// ---------------------------------------------------------------------------

fn gen_rustdoc_json(ws: &Path, krate: &str, private: bool) -> Result<PathBuf> {
    // Clean target/doc so the post-build glob finds exactly the one JSON we just
    // produced (robust to a crate whose [lib] name differs from its package).
    let doc_dir = ws.join("target/doc");
    let _ = std::fs::remove_dir_all(&doc_dir);
    let mut rustdoc_args = vec!["-Z", "unstable-options", "--output-format", "json"];
    if private {
        rustdoc_args.push("--document-private-items");
    }
    let status = Command::new("cargo")
        .args(["+nightly", "rustdoc", "-p", krate, "--manifest-path"])
        .arg(ws.join("Cargo.toml"))
        .arg("--")
        .args(&rustdoc_args)
        .status()
        .context("spawn cargo +nightly rustdoc (is a nightly toolchain installed?)")?;
    ensure!(
        status.success(),
        "rustdoc JSON generation failed for {krate}"
    );

    let mut jsons: Vec<PathBuf> = std::fs::read_dir(&doc_dir)
        .with_context(|| format!("read {}", doc_dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    match jsons.len() {
        1 => Ok(jsons.pop().unwrap()),
        n => bail!(
            "expected exactly one rustdoc JSON in {}, found {n}",
            doc_dir.display()
        ),
    }
}

fn gen_scip(ws: &Path) -> Result<PathBuf> {
    // Write under target/ (gitignored) so a crash can never leave the raw index
    // — which embeds an absolute project_root path — as a committable file.
    let target = ws.join("target");
    std::fs::create_dir_all(&target)?;
    let out = target.join("index.scip");
    let status = Command::new("rust-analyzer")
        .arg("scip")
        .arg(ws)
        .arg("--output")
        .arg(&out)
        .status()
        .context("spawn rust-analyzer scip (is rust-analyzer on PATH?)")?;
    ensure!(status.success(), "rust-analyzer scip failed");
    Ok(out)
}

fn write_json(path: &Path, v: &Value) -> Result<()> {
    let mut s = serde_json::to_string_pretty(v)?;
    s.push('\n');
    std::fs::write(path, s)?;
    Ok(())
}

fn rel(repo: &Path, p: &Path) -> String {
    p.strip_prefix(repo).unwrap_or(p).display().to_string()
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("tools/oracle-bless has two parents")
        .to_path_buf()
}
