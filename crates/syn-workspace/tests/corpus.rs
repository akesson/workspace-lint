//! Smoke gate for the public-crate corpus (ROADMAP Phase 2).
//!
//! Loads each real third-party crate vendored as a git submodule under `corpus/`
//! and asserts the resolver **survives code it didn't author**: `Workspace::load`
//! returns `Ok` (no panic), terminates within a generous ceiling, and produces a
//! non-trivial model (≥1 member, ≥1 parsed item). This is the highest-value gate
//! for dependency-free corpus crates — it catches resolver crashes / hangs on
//! real Rust that hand-authored fixtures never exercise.
//!
//! Each crate is copied to a tempdir before loading, so `cargo metadata`'s side
//! effects never dirty the read-only submodule checkout. `Workspace::load` runs
//! `cargo metadata --no-deps` (see `walk.rs`), so loading needs no network and no
//! resolvable dependency graph — the corpus crates are chosen dependency-light so
//! this holds.
//!
//! **Skips cleanly** when the submodules aren't checked out (fresh clone without
//! `git submodule update --init`, or the packaged crate), so the suite stays
//! green without the corpus present.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use syn_workspace::Workspace;
use tempfile::TempDir;

struct CorpusEntry {
    /// Display name (for failure messages).
    name: &'static str,
    /// Directory under `corpus/`.
    dir: &'static str,
}

/// The vendored corpus. Each entry is a git submodule pinned to a release SHA in
/// `.gitmodules`; add a crate here and as a submodule together.
const CORPUS: &[CorpusEntry] = &[
    CorpusEntry {
        name: "anyhow",
        dir: "anyhow",
    },
    CorpusEntry {
        name: "bitflags",
        dir: "bitflags",
    },
    CorpusEntry {
        name: "heck",
        dir: "heck",
    },
    CorpusEntry {
        name: "itertools",
        dir: "itertools",
    },
    // Deep, cfg-gated, arch-specific module tree (src/arch/{x86_64,aarch64,
    // wasm32,all,generic}/…): the structural stress test for module-file
    // resolution and `#[cfg(target_arch=…)]`-gated `mod`s + conditional `pub use`.
    CorpusEntry {
        name: "memchr",
        dir: "memchr",
    },
    // Multi-member workspace (thiserror lib + thiserror-impl proc-macro): the
    // first corpus crate with >1 member and a proc-macro target.
    CorpusEntry {
        name: "thiserror",
        dir: "thiserror",
    },
];

/// Termination guard, not a perf gate — shared CI runners are too noisy to assert
/// "sub-second" without flaking. A real hang (e.g. an infinite resolve loop on
/// some construct) trips this; normal loads are milliseconds.
const LOAD_CEILING: Duration = Duration::from_secs(30);

#[test]
fn corpus_smoke() {
    let root = corpus_root();
    let mut failures: Vec<String> = Vec::new();
    let mut ran = 0;

    for entry in CORPUS {
        let src = root.join(entry.dir);
        if !src.join("Cargo.toml").exists() {
            eprintln!(
                "corpus crate `{}` absent (no submodule?) — skipping",
                entry.name
            );
            continue;
        }
        ran += 1;
        if let Err(msg) = smoke_one(&src) {
            failures.push(format!("[{}] {msg}", entry.name));
        }
    }

    if ran == 0 {
        eprintln!("no corpus crates present — skipping (run `git submodule update --init`)");
        return;
    }
    assert!(
        failures.is_empty(),
        "corpus smoke failures ({}/{} crates):\n{}",
        failures.len(),
        ran,
        failures.join("\n")
    );
}

fn smoke_one(src: &Path) -> Result<(), String> {
    let tmp = TempDir::new().map_err(|e| format!("tempdir: {e}"))?;
    copy_tree(src, tmp.path()).map_err(|e| format!("copy: {e}"))?;

    let start = Instant::now();
    let ws = Workspace::load(tmp.path()).map_err(|e| format!("Workspace::load failed: {e}"))?;
    let elapsed = start.elapsed();
    eprintln!("  loaded {} in {elapsed:?}", src.display());
    if elapsed > LOAD_CEILING {
        return Err(format!("load took {elapsed:?} > ceiling {LOAD_CEILING:?}"));
    }

    let members = ws.members().count();
    if members == 0 {
        return Err("no workspace members materialized".into());
    }
    let items = ws.members().flat_map(|c| c.items()).count();
    if items == 0 {
        return Err("no items parsed across any member".into());
    }
    eprintln!("    {members} member(s), {items} item(s)");
    Ok(())
}

fn corpus_root() -> PathBuf {
    // crates/syn-workspace/ -> repo root -> corpus/
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("syn-workspace crate has a repo root two levels up")
        .join("corpus")
}

/// Recursively copy `src` into `dst`, pruning `.git` (submodule gitlink/metadata)
/// and `target` (build output) so the copy is cheap and the submodule checkout is
/// never mutated.
fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    let mut stack = vec![src.to_path_buf()];
    while let Some(path) = stack.pop() {
        let rel = path.strip_prefix(src).unwrap();
        let target = dst.join(rel);
        if path.is_dir() {
            std::fs::create_dir_all(&target)?;
            for entry in std::fs::read_dir(&path)?.flatten() {
                let name = entry.file_name();
                if name == "_git" || name == ".git" || name == "target" {
                    continue;
                }
                stack.push(entry.path());
            }
        } else {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&path, &target)?;
        }
    }
    Ok(())
}
