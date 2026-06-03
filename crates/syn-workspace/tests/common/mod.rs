//! Shared helpers for the syn-workspace differential/oracle harnesses
//! (`oracle.rs`, `scip_diff.rs`, `corpus.rs`): locate corpus crates and committed
//! oracle fixtures, copy a read-only submodule into a tempdir, and read the
//! normalized JSON oracles.
//!
//! Lives in `tests/common/mod.rs` (a subdirectory module, not a top-level
//! `tests/*.rs`) so cargo does not compile it as its own test target; each
//! harness pulls it in with `mod common;`. Helpers are `pub` and may be unused by
//! a given harness (e.g. `corpus.rs` uses only `copy_tree` + `corpus_root`), so
//! the module is `#![allow(dead_code)]`.

#![allow(dead_code)]

use serde_json::Value;
use std::path::{Path, PathBuf};

/// rust-analyzer emits UTF-8 code-unit (byte) column offsets; the byte-span
/// alignment the oracle nets rely on assumes it. Drift here fails loudly.
pub const EXPECTED_POSITION_ENCODING: &str = "UTF8CodeUnitOffsetFromLineStart";

/// The `corpus/` directory at the repo root (two levels up from this crate's
/// manifest dir): `crates/syn-workspace/` -> repo root -> `corpus/`.
pub fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("syn-workspace crate has a repo root two levels up")
        .join("corpus")
}

/// A committed oracle fixture under `tests/oracle/<name>/`.
pub fn fixture_dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/oracle")
        .join(name)
}

/// True when the `multi_crate` fixture's `workspace/` subtree is present. That
/// subtree is excluded from the packaged crate (it has its own `[workspace]`
/// table), so the oracle harnesses skip cleanly when run from a published
/// package rather than the source tree.
pub fn fixture_workspace_present(base: &Path) -> bool {
    base.join("workspace").exists()
}

/// Read and parse a committed JSON oracle artifact, panicking with a regen hint.
pub fn load_json(p: &Path) -> Value {
    let bytes = std::fs::read(p).unwrap_or_else(|e| {
        panic!("read oracle artifact {} ({e}); regenerate with `cargo run --manifest-path tools/oracle-bless/Cargo.toml`", p.display())
    });
    serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("parse {}: {e}", p.display()))
}

/// Recursively copy `src` into `dst`, pruning `.git`/`_git` (submodule
/// gitlink/metadata) and `target` (build output) so the copy is cheap and the
/// read-only submodule checkout is never mutated.
pub fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
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
