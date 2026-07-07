//! Golden-spine tier 1, glob-cleanup edition: pins the schema-8 facts the
//! glob-dangling accounting rests on, against `tests/probes/glob`:
//!
//! - **P1** — a glob `use` edge carries `decl_span` (byte-exact, the whole
//!   statement) and no `elem_span`; a resolver-unused glob carries an empty
//!   `glob_used_names`.
//! - **P2** (the design gate) — the glob_map records *every* resolution class
//!   the accounting needs name evidence for: a type by name, a `macro_rules!`
//!   by invocation, a derive by attribute.
//! - **P3** — the expansion-chain walk credits an outer macro that expands
//!   solely to an inner one.
//! - **P4** — a glob load-bearing only for trait-method syntax surfaces as a
//!   `trait_scope` edge to the glob's module (typeck `used_trait_imports`).
//! - **P5** — the nested brace-list glob's lowered `decl_span` shape, which
//!   the surgery guard's "whole statement or bail" test depends on.
//!
//! Same scaffolding as `probe.rs` (which documents the chdir/mtime/WL_IR_OUT
//! constraints); one `#[test]` per file, each integration-test binary its own
//! process.

use std::path::{Path, PathBuf};
use std::process::Command;

use dylint::opts::{Check, Dylint, LibrarySelection, Operation};
use wl_ir::{IrFragment, RefEdge};

fn read_fragment(path: &Path) -> anyhow::Result<IrFragment> {
    let bytes = std::fs::read(path)?;
    wl_ir::validate_header(&bytes).map_err(anyhow::Error::msg)?;
    Ok(wl_ir::from_archive_bytes(&bytes[wl_ir::HEADER_LEN..])?)
}

/// The on-disk text a span covers (spans are workspace-relative, byte-exact —
/// see `wl_ir::Span`).
fn slice(root: &Path, span: &wl_ir::Span) -> anyhow::Result<String> {
    let text = std::fs::read_to_string(root.join(&span.file))?;
    Ok(text
        .get(span.lo as usize..span.hi as usize)
        .unwrap_or("<out of range>")
        .to_string())
}

#[test]
fn glob_probe_accounting_facts() -> anyhow::Result<()> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let probe_root = manifest_dir.join("tests/probes/glob");

    let status = Command::new("cargo")
        .arg("build")
        .current_dir(&manifest_dir)
        .status()?;
    anyhow::ensure!(status.success(), "cargo build of the extractor failed");
    let lib_path = find_dylib(&manifest_dir.join("target/debug"))?;
    {
        let lib_file = std::fs::OpenOptions::new().append(true).open(&lib_path)?;
        lib_file.set_modified(std::time::SystemTime::now())?;
    }

    let ir_out = tempfile::tempdir()?;
    // SAFETY: single-threaded — this file holds exactly one test.
    unsafe { std::env::set_var("WL_IR_OUT", ir_out.path()) };
    std::env::set_current_dir(&probe_root)?;

    let opts = Dylint {
        pipe_stderr: None,
        pipe_stdout: None,
        quiet: false,
        operation: Operation::Check(Check {
            lib_sel: LibrarySelection {
                lib_paths: vec![lib_path.to_string_lossy().into_owned()],
                ..Default::default()
            },
            no_deps: true,
            ..Default::default()
        }),
    };
    dylint::run(&opts)?;

    let frag = read_fragment(&ir_out.path().join("probe_glob.wlir"))?;
    let globs: Vec<&RefEdge> = frag.references.iter().filter(|e| e.glob).collect();

    // --- P1 + P2: the load-bearing glob in `user` ---
    let user_glob = globs
        .iter()
        .find(|e| e.from.last().is_some_and(|m| m == "user"))
        .ok_or_else(|| anyhow::anyhow!("no glob edge from `user`; globs: {globs:#?}"))?;
    let decl = user_glob
        .decl_span
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("P1: glob edge lost its decl_span"))?;
    anyhow::ensure!(
        slice(&probe_root, decl)? == "use crate::prelude::*;",
        "P1: glob decl_span must slice the whole statement, got {:?}",
        slice(&probe_root, decl)?
    );
    anyhow::ensure!(
        user_glob.elem_span.is_none(),
        "P1: a glob has no excisable leaf — elem_span must stay None"
    );
    for name in ["ProbeDerive", "Widget", "widget"] {
        anyhow::ensure!(
            user_glob.glob_used_names.iter().any(|n| n == name),
            "P2: glob_map must record `{name}` (a {}), got {:?}",
            match name {
                "ProbeDerive" => "derive resolution",
                "widget" => "macro_rules! invocation",
                _ => "type name",
            },
            user_glob.glob_used_names
        );
    }

    // --- P1 (empty side): the resolver-unused glob ---
    let unused = globs
        .iter()
        .find(|e| e.from.last().is_some_and(|m| m == "unused_glob"))
        .ok_or_else(|| anyhow::anyhow!("no glob edge from `unused_glob`"))?;
    anyhow::ensure!(
        unused.glob_used_names.is_empty(),
        "P1: a never-used glob must carry an empty glob_used_names, got {:?}",
        unused.glob_used_names
    );
    anyhow::ensure!(
        unused.decl_span.is_some(),
        "P1: the unused glob still carries its delete surface"
    );

    // --- P3: expansion chain credits outer AND inner ---
    for mac in ["inner", "outer"] {
        anyhow::ensure!(
            frag.references.iter().any(|e| {
                !e.import
                    && e.from.last().is_some_and(|f| f == "chained")
                    && e.to.last().is_some_and(|t| t == mac)
            }),
            "P3: `chained` must carry a macro-use edge to `{mac}` (chain walk)"
        );
    }

    // --- P4: the trait-only glob surfaces as a trait_scope edge ---
    anyhow::ensure!(
        frag.references.iter().any(|e| {
            e.trait_scope
                && e.from.last().is_some_and(|f| f == "call")
                && e.to.last().is_some_and(|t| t == "prelude")
        }),
        "P4: `trait_user::call` must carry a trait_scope edge to the glob's module; \
         trait_scope edges: {:#?}",
        frag.references
            .iter()
            .filter(|e| e.trait_scope)
            .collect::<Vec<_>>()
    );
    // Discovered when this probe first ran (and pinned here on purpose): the
    // resolver records the TRAIT name in the glob_map even when the glob's
    // only job is method resolution — `record_use` fires during trait
    // candidate selection. So name evidence (R3/R5) covers trait usage too;
    // the trait_scope edge above remains the survivor-sensitive channel (the
    // glob_map is per-decl and resolution-time, blind to which user survives).
    let trait_glob = globs
        .iter()
        .find(|e| e.from.last().is_some_and(|m| m == "trait_user"))
        .ok_or_else(|| anyhow::anyhow!("no glob edge from `trait_user`"))?;
    anyhow::ensure!(
        trait_glob.glob_used_names.iter().any(|n| n == "Shout"),
        "P4: the resolver stopped recording trait-method resolutions in the \
         glob_map — R3/R5's name evidence no longer covers trait usage, got {:?}",
        trait_glob.glob_used_names
    );

    // --- P5: the nested brace-list glob's decl_span shape ---
    let nested = globs
        .iter()
        .find(|e| e.from.last().is_some_and(|m| m == "consumer"))
        .ok_or_else(|| anyhow::anyhow!("no glob edge from `consumer`"))?;
    let nested_decl = nested
        .decl_span
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("P5: nested glob edge lost its decl_span"))?;
    let nested_text = slice(&probe_root, nested_decl)?;
    anyhow::ensure!(
        !nested_text.starts_with("use "),
        "P5: a nested-list glob leaf must NOT present a whole-statement span \
         (the surgery guard bails on it); rustc's lowering changed — got {nested_text:?}"
    );

    Ok(())
}

/// Same discovery as `probe.rs`: dylint_linting's build script emits the
/// `@<toolchain>`-suffixed dylib next to the plain debug artifacts.
fn find_dylib(dir: &Path) -> anyhow::Result<PathBuf> {
    for entry in std::fs::read_dir(dir)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let prefixed = name.starts_with("libwl_extractor@") || name.starts_with("wl_extractor@");
        let dylib_ext = name.ends_with(".dylib") || name.ends_with(".so") || name.ends_with(".dll");
        if prefixed && dylib_ext {
            return Ok(entry.path());
        }
    }
    anyhow::bail!("no wl_extractor@<toolchain> dylib under {}", dir.display());
}
