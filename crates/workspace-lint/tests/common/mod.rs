//! Shared prelude for the spawn-the-binary integration harnesses (`cases.rs`,
//! `corpus_fp.rs`, `fix_fixtures.rs`, `integration.rs`, `cli_crate_version.rs`).
//! Reach for these instead of re-rolling the primitives in a new harness:
//!
//! - [`workspace_lint`] — the binary under test.
//! - [`copy_tree`] / [`walk_files`] — stage a workspace tree into a tempdir, and
//!   walk a tree for equality comparison.
//! - [`TestWorkspace`] — build a minimal cargo workspace + `.workspace-lint.toml`
//!   in a tempdir.
//! - [`snapshot_stderr`] — the normalize → (bless | compare) snapshot core.
//! - [`normalize_stderr`] — tempdir-path normalization for stable snapshots.
//!
//! Lives in `tests/common/mod.rs` (a subdirectory module, not a top-level
//! `tests/*.rs`) so cargo does not compile it as its own test target; each
//! harness pulls it in with `mod common;`. Helpers are `pub` and may be unused by
//! a given harness, so the module is `#![allow(dead_code)]`.

#![allow(dead_code)]

use assert_cmd::cargo::cargo_bin_cmd;
use std::path::{Path, PathBuf};

pub fn workspace_lint() -> assert_cmd::Command {
    cargo_bin_cmd!("workspace-lint")
}

pub fn bless_enabled() -> bool {
    std::env::var_os("WORKSPACE_LINT_BLESS").is_some()
}

/// Directory names pruned from every [`copy_tree`]: VCS metadata (`.git`, and
/// `_git` as some vendored submodules store it) and build output (`target`).
/// Static and shared so the former three hand-rolled `copy_tree` sites can't
/// drift apart again. Fixtures under `tests/cases` contain none of these, so
/// pruning is a no-op there; it only matters when copying a real corpus crate.
const COPY_PRUNE_DIRS: &[&str] = &[".git", "_git", "target"];

pub fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    if !src.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("source missing: {}", src.display()),
        ));
    }
    for entry in walkdir(src) {
        let entry = entry?;
        let rel = entry.strip_prefix(src).unwrap();
        let target = dst.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&target)?;
        } else {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&entry, &target)?;
        }
    }
    Ok(())
}

pub fn walkdir(root: &Path) -> impl Iterator<Item = std::io::Result<PathBuf>> + use<> {
    // Minimal recursive iterator: avoids pulling in the walkdir crate. Prunes
    // `COPY_PRUNE_DIRS` (VCS metadata + build output) so copying a real corpus
    // crate is cheap and never touches its checkout — fixtures under tests/cases
    // have none of those, so this is a no-op there.
    let mut stack = vec![root.to_path_buf()];
    std::iter::from_fn(move || {
        let path = stack.pop()?;
        if path.is_dir() {
            let read = match std::fs::read_dir(&path) {
                Ok(r) => r,
                Err(e) => return Some(Err(e)),
            };
            for entry in read.flatten() {
                let name = entry.file_name();
                if COPY_PRUNE_DIRS.iter().any(|p| name == *p) {
                    continue;
                }
                stack.push(entry.path());
            }
        }
        Some(Ok(path))
    })
}

pub fn normalize_stderr(stderr: &str, tmp: &Path) -> String {
    // Build every reasonable spelling of the tempdir path:
    //
    // - `tmp.path()` and `tmp.canonicalize()` to handle macOS' /var → /private/var
    //   symlink dance, and the short-vs-long-name distinction on Windows
    //   (`RUNNER~1` vs `runneradmin`).
    // - Forward-slash forms of both, since the renderer normalizes paths to
    //   forward-slash on Windows but `Path::to_string_lossy()` still gives us
    //   backslashes here.
    // - Verbatim-prefix-stripped (`\\?\`) variants in case `canonicalize` ever
    //   returns one (currently it doesn't reach our stderr — the renderer
    //   strips it — but defending against it costs nothing).
    //
    // Sort by length descending and replace in that order: longer paths
    // (e.g. /private/var/folders/...) must consume their content before the
    // shorter alias (/var/folders/...) gets a chance, otherwise we leave a
    // stray prefix behind.
    let mut spellings: Vec<String> = Vec::new();
    let push = |spellings: &mut Vec<String>, s: String| {
        if !s.is_empty() && !spellings.contains(&s) {
            spellings.push(s);
        }
    };
    push(&mut spellings, tmp.to_string_lossy().into_owned());
    if let Ok(canon) = tmp.canonicalize() {
        push(&mut spellings, canon.to_string_lossy().into_owned());
    }
    let with_fs: Vec<String> = spellings.iter().map(|s| s.replace('\\', "/")).collect();
    for s in with_fs {
        push(&mut spellings, s);
    }
    let stripped: Vec<String> = spellings
        .iter()
        .filter_map(|s| s.strip_prefix(r"\\?\").map(|t| t.to_string()))
        .collect();
    for s in stripped {
        push(&mut spellings, s);
    }
    spellings.sort_by_key(|s| std::cmp::Reverse(s.len()));

    let mut out = stderr.to_string();
    for s in &spellings {
        out = out.replace(s.as_str(), "<TMP>");
    }
    // Normalize line endings: on Windows, git's `core.autocrlf` can rewrite
    // committed `expected.stderr` to CRLF on checkout while the captured
    // subprocess output stays LF. Comparing post-normalization keeps the
    // snapshots cross-platform without requiring a .gitattributes rule.
    out.replace("\r\n", "\n")
}

/// Every regular file under `root`, as paths relative to `root`, sorted — for
/// tree-equality comparison (`fix_fixtures.rs`). Prunes `target/` directories
/// and any file whose name is in `exclude_files` (e.g. `["Cargo.lock"]`, a
/// `cargo metadata` side-effect that isn't part of the user-visible workspace
/// state under test).
///
/// Note the deliberate split from [`COPY_PRUNE_DIRS`]: *copying* prunes VCS/build
/// dirs, *comparing* prunes a build side-effect file. They are different concerns
/// and must never share one list.
pub fn walk_files(root: &Path, exclude_files: &[&str]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if !root.is_dir() {
        return out;
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if path.is_dir() {
                if name == "target" {
                    continue;
                }
                stack.push(path);
            } else if path.is_file() {
                if exclude_files.contains(&name) {
                    continue;
                }
                if let Ok(rel) = path.strip_prefix(root) {
                    out.push(rel.to_path_buf());
                }
            }
        }
    }
    out.sort();
    out
}

/// Comparison outcome from [`snapshot_stderr`]. The normalized actual stderr is
/// returned separately (see the function) so a caller can build its own failure
/// message and reuse the same `actual` for an unrelated report — `cases.rs`
/// embeds it in an exit-code mismatch even when the stderr itself matched.
pub enum SnapshotResult {
    Blessed,
    Match,
    Mismatch { expected: String },
}

/// Normalize captured `stderr_raw` for the tempdir `tmp`, then bless-or-compare
/// against `expected_path`. Returns `(normalized_actual, outcome)` — the single
/// source of truth for the normalize → (write | trim-equal) snapshot core shared
/// by `cases.rs` and `corpus_fp.rs`.
///
/// It deliberately does NOT spawn the binary, copy the tree, or inspect the exit
/// code — the caller owns those so it keeps its own setup, env, and exit-code
/// policy (only `cases.rs` checks the exit code against its case `Kind`).
pub fn snapshot_stderr(
    stderr_raw: &[u8],
    tmp: &Path,
    expected_path: &Path,
    bless: bool,
) -> std::io::Result<(String, SnapshotResult)> {
    let actual = normalize_stderr(&String::from_utf8_lossy(stderr_raw), tmp);
    if bless {
        if let Some(parent) = expected_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(expected_path, &actual)?;
        return Ok((actual, SnapshotResult::Blessed));
    }
    let expected = std::fs::read_to_string(expected_path)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    let outcome = if expected.trim() == actual.trim() {
        SnapshotResult::Match
    } else {
        SnapshotResult::Mismatch { expected }
    };
    Ok((actual, outcome))
}

/// A member crate inside a [`TestWorkspace`].
struct MemberCrate {
    /// Directory relative to the workspace root (e.g. `"crates/demo"`); also the
    /// entry registered in the root `[workspace] members` list.
    rel_dir: String,
    /// Full `Cargo.toml` body, written verbatim.
    cargo_toml: String,
    /// `(path-within-crate, contents)` source files (e.g. `("src/lib.rs", "…")`).
    src: Vec<(String, String)>,
}

/// Builds a minimal cargo workspace (+ optional `Cargo.lock` and
/// `.workspace-lint.toml`) into a caller-owned tempdir. Replaces the hand-rolled
/// scaffolding that used to live in `integration.rs` and `cli_crate_version.rs`.
///
/// Manifests and the lockfile are written **verbatim** — cargo's manifest format
/// has enough significant ordering/whitespace that templating it is more fragile
/// than passing the exact string a harness already hand-wrote. Only
/// [`Self::lib_member`] templates, and only the ubiquitous "one lib crate" shape.
#[derive(Default)]
pub struct TestWorkspace {
    members: Vec<String>,
    resolver: Option<&'static str>,
    member_crates: Vec<MemberCrate>,
    loose_files: Vec<(String, String)>,
    lock: Option<String>,
    config: Option<String>,
}

impl TestWorkspace {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the workspace `resolver` (serialized before `members`).
    pub fn resolver(mut self, r: &'static str) -> Self {
        self.resolver = Some(r);
        self
    }

    /// Add a member crate with a verbatim `cargo_toml` and source files,
    /// registering `rel_dir` in the `[workspace] members` list.
    pub fn member(
        mut self,
        rel_dir: &str,
        cargo_toml: impl Into<String>,
        src: &[(&str, &str)],
    ) -> Self {
        self.members.push(rel_dir.to_string());
        self.member_crates.push(MemberCrate {
            rel_dir: rel_dir.to_string(),
            cargo_toml: cargo_toml.into(),
            src: src
                .iter()
                .map(|(p, c)| (p.to_string(), c.to_string()))
                .collect(),
        });
        self
    }

    /// Convenience for the common "one lib crate, edition 2024, this `lib.rs`"
    /// shape. The manifest format matches what the call sites hand-wrote.
    pub fn lib_member(self, rel_dir: &str, name: &str, version: &str, lib_rs: &str) -> Self {
        let manifest = format!(
            "[package]\nname = \"{name}\"\nversion = \"{version}\"\nedition = \"2024\"\n\n[lib]\npath = \"src/lib.rs\"\n"
        );
        self.member(rel_dir, manifest, &[("src/lib.rs", lib_rs)])
    }

    /// Add a file with no owning crate (e.g. a loose `src/big.rs` at the
    /// workspace root, not listed in `members`).
    pub fn loose_file(mut self, rel: &str, contents: &str) -> Self {
        self.loose_files
            .push((rel.to_string(), contents.to_string()));
        self
    }

    /// Set the full `Cargo.lock` body (written verbatim).
    pub fn lock(mut self, body: impl Into<String>) -> Self {
        self.lock = Some(body.into());
        self
    }

    /// Set the full `.workspace-lint.toml` body (written verbatim).
    pub fn config(mut self, body: impl Into<String>) -> Self {
        self.config = Some(body.into());
        self
    }

    /// Materialize the workspace into `dir`. Panics on any IO error — matching
    /// the `.unwrap()`-heavy call sites this replaces.
    pub fn write(self, dir: &Path) {
        let mut root = String::from("[workspace]\n");
        if let Some(r) = self.resolver {
            root.push_str(&format!("resolver = \"{r}\"\n"));
        }
        let members = self
            .members
            .iter()
            .map(|m| format!("\"{m}\""))
            .collect::<Vec<_>>()
            .join(", ");
        root.push_str(&format!("members = [{members}]\n"));
        std::fs::write(dir.join("Cargo.toml"), root).expect("write root Cargo.toml");

        for m in &self.member_crates {
            let crate_dir = dir.join(&m.rel_dir);
            std::fs::create_dir_all(&crate_dir).expect("create member dir");
            std::fs::write(crate_dir.join("Cargo.toml"), &m.cargo_toml)
                .expect("write member Cargo.toml");
            for (rel, contents) in &m.src {
                write_with_parents(&crate_dir.join(rel), contents);
            }
        }

        for (rel, contents) in &self.loose_files {
            write_with_parents(&dir.join(rel), contents);
        }

        if let Some(lock) = &self.lock {
            std::fs::write(dir.join("Cargo.lock"), lock).expect("write Cargo.lock");
        }
        if let Some(config) = &self.config {
            std::fs::write(dir.join(".workspace-lint.toml"), config)
                .expect("write .workspace-lint.toml");
        }
    }
}

fn write_with_parents(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent dir");
    }
    std::fs::write(path, contents).expect("write file");
}
