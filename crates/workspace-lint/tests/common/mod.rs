//! Shared helpers for the spawn-the-binary integration harnesses (`cases.rs`,
//! `corpus_fp.rs`): copy a workspace tree into a tempdir, run the
//! `workspace-lint` binary in it, and normalize its stderr for snapshotting.
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
    // `.git` (submodule gitlink/metadata) and `target` (build output) so copying
    // a real corpus crate is cheap and never touches its checkout — fixtures
    // under tests/cases have neither, so this is a no-op there.
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
                if name == ".git" || name == "target" {
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
