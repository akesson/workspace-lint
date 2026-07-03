//! Corpus coverage: real third-party crates vendored as git submodules under
//! `corpus/` (fixture code exercises shapes hand-written fixtures don't).
//!
//! Two tiers, mirroring the binary's own:
//!
//! - [`corpus_fast_tier_smoke`] — in the default suite: the build-free lints
//!   (`--fast-only`) over EVERY checked-out corpus crate. No compilation, no
//!   network (`FastModel` loads via `cargo metadata --no-deps`), so it runs
//!   wherever the submodules are present and **skips cleanly** where they
//!   aren't (a plain `git clone` without `--recurse-submodules`).
//! - [`corpus_full_tier_subset`] — `#[ignore]`d: the full engine (extraction
//!   on the pinned toolchain + assembly + the semantic lints) over the
//!   single-crate corpus repos, compiling them for real — which needs their
//!   registry deps and therefore network on a cold cache. Run by the
//!   scheduled `corpus` workflow (`.github/workflows/corpus.yml`), or
//!   locally via `cargo test --test corpus -- --ignored`.
//!
//! Both tiers assert the run *completes with exit 0*: findings are expected
//! (real-world code, warn level) — the corpus guards against crashes, loud
//! model failures, and deny-level regressions, while the fixture taxonomy in
//! `tests/cases/` carries the per-finding FP/FN contract.

use std::path::{Path, PathBuf};

use tempfile::TempDir;

mod common;
use common::{copy_tree, workspace_lint};

/// The single-crate repos the scheduled full-tier run compiles. The larger
/// monorepos (dioxus, regex, itertools) stay fast-tier-only: their dep trees
/// and member counts make a scheduled compile slow without adding engine
/// coverage the small crates don't already give.
const FULL_TIER_SUBSET: &[&str] = &["anyhow", "bitflags", "heck", "memchr", "thiserror"];

fn corpus_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus")
}

/// Every corpus submodule that is actually checked out (has a `Cargo.toml`).
fn checked_out() -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(corpus_root()) else {
        return Vec::new();
    };
    let mut crates: Vec<(String, PathBuf)> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("Cargo.toml").is_file())
        .map(|p| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            (name, p)
        })
        .collect();
    crates.sort();
    crates
}

/// Copy a corpus crate to a tempdir (runs must not dirty the submodule — a
/// stray `Cargo.lock` or `target/` would show up in `git status` forever),
/// write the given config, and run the binary there, asserting completion
/// with exit 0.
fn run_in_copy(name: &str, src: &Path, config: &str, args: &[&str]) {
    let tmp = TempDir::new().expect("create tempdir");
    copy_tree(src, tmp.path()).expect("copy corpus crate");
    std::fs::write(tmp.path().join(".workspace-lint.toml"), config).expect("write corpus config");
    let output = workspace_lint()
        .current_dir(tmp.path())
        .args(args)
        .output()
        .expect("spawn workspace-lint");
    assert!(
        output.status.success(),
        "[{name}] workspace-lint exited {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn corpus_fast_tier_smoke() {
    let crates = checked_out();
    if crates.is_empty() {
        eprintln!("corpus submodules not checked out — skipping (git submodule update --init)");
        return;
    }
    // A run refuses to start without a config file; an empty one means
    // "structural lints at their defaults".
    for (name, path) in &crates {
        run_in_copy(name, path, "[lints]\n", &["--fast-only"]);
        eprintln!("corpus fast-tier OK: {name}");
    }
}

#[test]
#[ignore = "compiles corpus crates (registry deps, pinned toolchain); run by the scheduled corpus workflow"]
fn corpus_full_tier_subset() {
    // Semantic lints are policy lints — without a config table they don't
    // run and the engine never engages. Warn level keeps real-world findings
    // (these crates were never linted with this tool) from failing the exit.
    const CONFIG: &str = "\
[unused-deps]\n\
[unused-pub]\n\
\n\
[engine]\n\
configs = [\"default\"]\n\
\n\
[lints]\n\
default = \"allow\"\n\
unused-deps = \"warn\"\n\
unused-pub = \"warn\"\n";
    let root = corpus_root();
    for name in FULL_TIER_SUBSET {
        let path = root.join(name);
        assert!(
            path.join("Cargo.toml").is_file(),
            "[{name}] submodule not checked out — the scheduled workflow checks out recursively",
        );
        run_in_copy(name, &path, CONFIG, &[]);
        eprintln!("corpus full-tier OK: {name}");
    }
}
