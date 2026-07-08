//! Git working-tree gate for `--fix`, and the one place git is ever spawned.
//!
//! `--fix` mutates source files in place — structural rewrites applied as
//! byte-range replacements (never written suppression directives). To
//! keep the whole change reviewable as a single `git diff`, `--fix` refuses to
//! run when the working tree has uncommitted modifications to **tracked**
//! files, unless `--allow-dirty` is passed. Untracked files are fine — our
//! writes only touch existing tracked manifests and sources, so they can't
//! clobber unsaved work.
//!
//! When the directory is not a git repository at all, the gate **warns and
//! proceeds**: there is nothing to clobber and no diff to protect, and this
//! keeps the tempdir-based fix fixtures (which aren't repos) working. This is a
//! deliberate departure from `cargo fix`, which requires `--allow-no-vcs`.

use std::path::Path;
use std::process::Command;

/// Environment variables that pin git to a specific repository, mirroring
/// git's own `local_repo_env` list (environment.c). Git exports an absolute
/// `GIT_DIR` to hook processes when the repository is discovered via a `.git`
/// *file* (linked worktrees, submodules); a child git inheriting it operates
/// on the *invoker's* repository — with the child's cwd as its work tree —
/// instead of the directory we point it at. Observed in the wild: running the
/// test suite from a pre-push hook in a linked worktree committed fixture
/// trees onto the developer's real branch. Every git spawn must scrub these.
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
/// environment (see `GIT_REPO_ENV`) so the repository is always discovered
/// from `dir` itself. All git spawns in this crate must go through here —
/// never `Command::new("git")` directly.
pub fn command(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir);
    for var in GIT_REPO_ENV {
        cmd.env_remove(var);
    }
    cmd
}

/// The relevant state of a git working tree for the `--fix` gate.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TreeState {
    /// No tracked-file modifications — safe to proceed.
    Clean,
    /// Not a git repository (or git is unavailable). The gate proceeds with a
    /// warning since there is no diff to protect.
    NotARepo,
    /// Tracked files have uncommitted changes. Carries the `git status
    /// --porcelain` lines so the error can list them.
    Dirty(Vec<String>),
}

/// Classify `dir`'s git working tree. Runs `git status --porcelain` in `dir`;
/// a spawn failure (git missing) or non-zero exit (not a repo) both map to
/// [`TreeState::NotARepo`]. Porcelain lines starting with `??` are untracked
/// and ignored; any other non-empty line is a tracked modification.
pub(crate) fn tree_state(dir: &Path) -> TreeState {
    let out = match command(dir).args(["status", "--porcelain"]).output() {
        Ok(o) => o,
        Err(_) => return TreeState::NotARepo, // git binary unavailable
    };
    if !out.status.success() {
        return TreeState::NotARepo; // `fatal: not a git repository`, etc.
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let dirty: Vec<String> = text
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with("??"))
        .map(str::to_string)
        .collect();
    if dirty.is_empty() {
        TreeState::Clean
    } else {
        TreeState::Dirty(dirty)
    }
}

/// Enforce the clean-tree precondition for `--fix`. `--allow-dirty` bypasses
/// the check entirely. A dirty tracked tree exits with code 2 and a message
/// listing the offending files; a missing repo warns and returns.
pub fn ensure_clean_for_fix(dir: &Path, allow_dirty: bool) {
    if allow_dirty {
        return;
    }
    match tree_state(dir) {
        TreeState::Clean => {}
        TreeState::NotARepo => {
            eprintln!(
                "workspace-lint --fix: not a git repository; proceeding without a clean-tree guard"
            );
        }
        TreeState::Dirty(files) => {
            eprintln!(
                "error: `--fix` needs a clean git working tree so its changes\n\
                 (structural fixes and any auto-written `expect` directives) are\n\
                 reviewable as a single `git diff`. These tracked files have\n\
                 uncommitted changes:"
            );
            for f in &files {
                eprintln!("  {f}");
            }
            eprintln!("Commit or `git stash` them first, or pass --allow-dirty to override.");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Initialize a git repo in `dir` with a committed file, so the tree starts
    /// clean. Returns once the initial commit exists.
    fn init_repo(dir: &Path) {
        let run = |args: &[&str]| {
            let ok = command(dir)
                .args(args)
                .output()
                .expect("git available")
                .status
                .success();
            assert!(ok, "git {args:?} failed");
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@t.test"]);
        run(&["config", "user.name", "Test"]);
        std::fs::write(dir.join("tracked.txt"), "v1\n").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-q", "-m", "init"]);
    }

    #[test]
    fn clean_repo_is_clean() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        assert_eq!(tree_state(tmp.path()), TreeState::Clean);
    }

    #[test]
    fn modified_tracked_file_is_dirty() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        std::fs::write(tmp.path().join("tracked.txt"), "v2\n").unwrap();
        match tree_state(tmp.path()) {
            TreeState::Dirty(lines) => {
                assert!(lines.iter().any(|l| l.contains("tracked.txt")));
            }
            other => panic!("expected Dirty, got {other:?}"),
        }
    }

    #[test]
    fn untracked_file_only_is_clean() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        std::fs::write(tmp.path().join("new.txt"), "hello\n").unwrap();
        assert_eq!(
            tree_state(tmp.path()),
            TreeState::Clean,
            "an untracked file must not trip the gate"
        );
    }

    #[test]
    fn non_repo_is_not_a_repo() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(tree_state(tmp.path()), TreeState::NotARepo);
    }

    #[test]
    fn command_scrubs_repo_env() {
        let tmp = TempDir::new().unwrap();
        let cmd = command(tmp.path());
        // `env_remove` shows up in `get_envs()` as a `None` value. Every
        // repo-pinning variable must be scheduled for removal, or an inherited
        // GIT_DIR (exported by git to hooks in linked worktrees) redirects the
        // spawn to the invoker's repository.
        let removed: Vec<&std::ffi::OsStr> = cmd
            .get_envs()
            .filter_map(|(k, v)| v.is_none().then_some(k))
            .collect();
        for var in GIT_REPO_ENV {
            assert!(
                removed.iter().any(|k| *k == *var),
                "{var} must be scrubbed from git spawns"
            );
        }
    }
}
