//! Shared prelude for the spawn-the-binary integration harnesses (`cases.rs`,
//! `corpus_fp.rs`, `fix_fixtures.rs`, `integration.rs`, `cli_crate_version.rs`).
//! Reach for these instead of re-rolling the primitives in a new harness:
//!
//! - [`workspace_lint`] — the binary under test.
//! - [`git`] — spawn git in a fixture dir; the ONLY way tests may run git.
//! - [`copy_tree`] / [`walk_files`] — stage a workspace tree into a tempdir, and
//!   walk a tree for equality comparison.
//! - [`Kind`] / [`walk_cases`] / [`cases_root`] — the `tests/cases/` taxonomy
//!   and its discovery walk, shared by `cases.rs` and `fixture_compile.rs`.
//! - [`apply_setup`] — apply a fixture's optional `setup.toml` (git index, file
//!   mtimes, uncommitted directive injection) to a staged tempdir; shared by
//!   `cases.rs` and `fix_fixtures.rs`.
//! - [`SEMANTIC_LINTS`] / [`lint_needs_build`] — routing metadata: which lints'
//!   fixtures are analyzed by the compiling (rustc-driver) engine backend.
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

/// Environment variables that pin git to a specific repository, mirroring
/// git's own `local_repo_env` (kept in sync with `GIT_REPO_ENV` in
/// `src/git.rs` — duplicated because the binary crate has no lib target to
/// import from). Git exports an absolute `GIT_DIR` to hook processes when the
/// repo is discovered via a `.git` *file* (linked worktrees, submodules); a
/// test-spawned git inheriting it operates on the *developer's* repository
/// with the fixture tempdir as its work tree. That exact leak — the suite run
/// by the pre-push hook from a linked worktree — once committed fixture trees
/// onto the real branch and flipped the shared config to `core.bare = true`.
const GIT_REPO_ENV: &[&str] = &[
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_CONFIG",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS",
    "GIT_DIR",
    "GIT_GRAFT_FILE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_OBJECT_DIRECTORY",
    "GIT_PREFIX",
    "GIT_REPLACE_REF_BASE",
    "GIT_SHALLOW_FILE",
    "GIT_WORK_TREE",
];

/// Build a `git` invocation rooted at `dir`, scrubbed of the repo-pinning
/// environment (see [`GIT_REPO_ENV`]) and isolated from the developer's
/// global/system git config (`commit.gpgsign`, `core.hooksPath`,
/// `init.templateDir`, … would otherwise leak into fixture repos). All git
/// spawns in tests must go through here — never `Command::new("git")`
/// directly.
pub fn git(dir: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new("git");
    cmd.current_dir(dir);
    for var in GIT_REPO_ENV {
        cmd.env_remove(var);
    }
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null");
    cmd.env("GIT_CONFIG_NOSYSTEM", "1");
    cmd
}

pub fn bless_enabled() -> bool {
    std::env::var_os("WORKSPACE_LINT_BLESS").is_some()
}

/// Apply an optional `setup.toml` (sibling of the fixture's staged tree) to the
/// copied tempdir before the binary runs. Lets fixtures that need state which
/// can't be committed inert — an injected `# workspace-lint:` directive, or a
/// deliberately dirty file — stay dogfood-safe: a directive committed under
/// `tests/**` would be picked up by this repo's own scan. Returns extra CLI
/// args for the binary invocation. Shared by `cases.rs` and `fix_fixtures.rs`.
///
/// Every staged tree is git-init'd and committed by default (AFTER `[[append]]`,
/// so injected directives are committed too): deletion suggestions are
/// `MachineApplicable` only for a tracked-and-clean file, so a non-repo tempdir
/// would silently withhold every deletion and grow "commit first" notes in the
/// snapshots. `[git] init = false` opts a fixture out (to exercise the
/// no-repo-⇒-withhold behavior itself); `[[append_after_commit]]` dirties a
/// file *after* the commit (to exercise the dirty-file withhold). Schema:
///
/// ```toml
/// args = ["--fast-only"]      # extra CLI args for the binary
///
/// [git]                       # staged trees are init+committed by default
/// init = false                # opt out: run in a plain (non-repo) tempdir
///
/// [[append]]                  # inject a directive that must stay out of the
/// path = "crates/demo/Cargo.toml"     # committed FIXTURE (it is committed in
/// text = "\n# workspace-lint: expect(centralized-deps)\n"  # the tempdir repo)
///
/// [[append_after_commit]]     # dirty a tracked file post-commit
/// path = "crates/demo/Cargo.toml"
/// text = "\n# a local edit\n"
/// ```
pub fn apply_setup(fixture_dir: &Path, tmp: &Path) -> Result<Vec<String>, String> {
    let setup_path = fixture_dir.join("setup.toml");
    let doc: toml::Value = if setup_path.exists() {
        let text =
            std::fs::read_to_string(&setup_path).map_err(|e| format!("read setup.toml: {e}"))?;
        toml::from_str(&text).map_err(|e| format!("parse setup.toml: {e}"))?
    } else {
        toml::Value::Table(Default::default())
    };

    // Append text to a file *after* copy — used to inject a `# workspace-lint:`
    // directive that must reach the run but stay out of the committed fixture
    // (otherwise this repo's own dogfood scan would pick the directive up from
    // tests/ and trip stale-expect / unknown-lint). Runs BEFORE the git commit
    // so the injected text is tracked-and-clean like the rest of the tree.
    apply_appends(&doc, "append", tmp)?;

    // Init + commit unless the fixture opts out. The committed repo is what
    // makes deletion suggestions `MachineApplicable` (git is the backup).
    if doc
        .get("git")
        .and_then(|g| g.get("init"))
        .and_then(toml::Value::as_bool)
        != Some(false)
    {
        git_cmd(tmp, &["init", "-q"])?;
        git_cmd(tmp, &["add", "-A"])?;
        git_cmd(
            tmp,
            &[
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-q",
                "-m",
                "setup",
            ],
        )?;
    }

    // Post-commit appends: leave a tracked file deliberately dirty.
    apply_appends(&doc, "append_after_commit", tmp)?;

    // Extra CLI args for the binary — lets a fixture exercise a flagged run
    // (e.g. `--fast-only`) while staying in the standard taxonomy.
    let mut args = Vec::new();
    if let Some(entries) = doc.get("args").and_then(toml::Value::as_array) {
        for entry in entries {
            args.push(
                entry
                    .as_str()
                    .ok_or("args entries must be strings")?
                    .to_string(),
            );
        }
    }

    Ok(args)
}

/// Process one `[[append]]`-shaped array (`key`): append each entry's `text`
/// to its `path` under `tmp`.
fn apply_appends(doc: &toml::Value, key: &str, tmp: &Path) -> Result<(), String> {
    let Some(entries) = doc.get(key).and_then(toml::Value::as_array) else {
        return Ok(());
    };
    for entry in entries {
        let rel = entry
            .get("path")
            .and_then(toml::Value::as_str)
            .ok_or_else(|| format!("{key} entry needs a string `path`"))?;
        let text = entry
            .get("text")
            .and_then(toml::Value::as_str)
            .ok_or_else(|| format!("{key} entry needs a string `text`"))?;
        let p = tmp.join(rel);
        let mut content =
            std::fs::read_to_string(&p).map_err(|e| format!("{key} read {rel}: {e}"))?;
        content.push_str(text);
        std::fs::write(&p, content).map_err(|e| format!("{key} write {rel}: {e}"))?;
    }
    Ok(())
}

fn git_cmd(dir: &Path, args: &[&str]) -> Result<(), String> {
    let out = git(dir)
        .args(args)
        .output()
        .map_err(|e| format!("git {args:?}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

/// The semantic lints being ported onto the compiling (rustc-driver) engine
/// backend. Their case fixtures are analyzed by an engine that `cargo check`s
/// the fixture workspace, so `fixture_compile.rs` gates that every one of
/// their `tests/cases/<lint>/**/workspace/` trees compiles offline.
///
/// Keep in sync with `LintId` in `src/lints/lints_id.rs` — not importable
/// here (the crate is binary-only). The drift guard is
/// `cases.rs::semantic_lint_routing_matches_case_dirs`, which pins each entry
/// to its `tests/cases/<lint>/` directory; those directory names are in turn
/// pinned to `LintId::short()` by lints_id.rs's `every_lint_has_case_fixtures`.
pub const SEMANTIC_LINTS: &[&str] = &[
    "architecture",
    "duplicate-code",
    "orphan-file",
    "unused-deps",
    "unused-pub",
];

/// Case-routing metadata: `true` if `lint_dir` (a `tests/cases/<lint_dir>/`
/// name) belongs to a lint whose backend compiles the fixture workspace.
/// `fixture_compile.rs` uses it to pick which cases to sweep; the case
/// harness itself will use it to route fast vs semantic runs once the
/// rustc-backed lint ports land.
pub fn lint_needs_build(lint_dir: &str) -> bool {
    SEMANTIC_LINTS.contains(&lint_dir)
}

/// The four-kind outcome taxonomy under `tests/cases/<lint>/<kind>/<case>/`;
/// see `cases.rs` for the layout contract and what each kind asserts.
#[derive(Debug, Clone, Copy)]
pub enum Kind {
    TruePositive,
    TrueNegative,
    KnownFalsePositive,
    KnownFalseNegative,
}

impl Kind {
    pub fn from_dir_name(name: &str) -> Option<Self> {
        match name {
            "true_positives" => Some(Self::TruePositive),
            "true_negatives" => Some(Self::TrueNegative),
            "known_false_positives" => Some(Self::KnownFalsePositive),
            "known_false_negatives" => Some(Self::KnownFalseNegative),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::TruePositive => "TP",
            Self::TrueNegative => "TN",
            Self::KnownFalsePositive => "KFP",
            Self::KnownFalseNegative => "KFN",
        }
    }

    /// Exit-code policy applied by `cases.rs`: kinds where the lint flags
    /// something must exit non-zero.
    pub fn expects_failure_exit(self) -> bool {
        matches!(self, Self::TruePositive | Self::KnownFalsePositive)
    }
}

/// `tests/cases/` in the source tree (not a tempdir copy).
pub fn cases_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("cases")
}

/// Walk every `tests/cases/<lint>/<kind>/<case>/` directory, invoking
/// `visit(lint, kind, case_dir)`. Directories whose kind name isn't one of
/// the four taxonomy buckets are skipped. Shared by `cases.rs` (runs each
/// case) and `fixture_compile.rs` (offline-compiles the semantic subset).
pub fn walk_cases(mut visit: impl FnMut(&str, Kind, &Path)) {
    let root = cases_root();
    if !root.exists() {
        return;
    }
    for lint_entry in std::fs::read_dir(&root).expect("read cases/").flatten() {
        let lint_dir = lint_entry.path();
        if !lint_dir.is_dir() {
            continue;
        }
        let lint = lint_dir.file_name().and_then(|s| s.to_str()).unwrap_or("");
        for kind_entry in std::fs::read_dir(&lint_dir).expect("read kind/").flatten() {
            let kind_dir = kind_entry.path();
            let Some(kind_name) = kind_dir.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            let Some(kind) = Kind::from_dir_name(kind_name) else {
                continue;
            };
            for case_entry in std::fs::read_dir(&kind_dir).expect("read case/").flatten() {
                let case_dir = case_entry.path();
                if case_dir.is_dir() {
                    visit(lint, kind, &case_dir);
                }
            }
        }
    }
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
    let out = out.replace("\r\n", "\n");
    // Drop the engine's per-config progress line: dylint prints "Checking
    // with toolchain `nightly-…-<host triple>`" straight to stderr (its
    // Builder asserts a `check` can never be quieted), the triple is
    // platform-dependent, and the line count varies with the `[engine]`
    // config matrix — all three make it unsnapshottable. It's deliberate
    // user-facing progress, so it's filtered here rather than suppressed.
    out.lines()
        .filter(|l| !l.starts_with("Checking with toolchain `"))
        .map(|l| format!("{l}\n"))
        .collect()
}

/// Every regular file under `root`, as paths relative to `root`, sorted — for
/// tree-equality comparison (`fix_fixtures.rs`). Prunes the same VCS/build
/// directories as [`COPY_PRUNE_DIRS`] (`.git`, `_git`, `target`) plus any file
/// whose name is in `exclude_files` (e.g. `["Cargo.lock"]`, a `cargo metadata`
/// side-effect that isn't part of the user-visible workspace state under test).
///
/// The `.git` prune matters for a fixture whose `setup.toml` does `[git] init`
/// (deletion under `--fix-auto-delete` needs a clean repo): the staged tempdir then carries a `.git/`
/// whose commit hashes are non-deterministic — never a comparison target. The
/// dir-prune list is shared with copying; only the *file* exclusion differs
/// (comparing drops the generated `Cargo.lock`, copying keeps everything else).
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
                if COPY_PRUNE_DIRS.contains(&name) {
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
