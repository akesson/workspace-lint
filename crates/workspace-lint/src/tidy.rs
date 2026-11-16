//! Keep a fmt-clean workspace fmt-clean across `--fix`.
//!
//! A fix is a byte-range rewrite with no idea of layout: `pub fn` →
//! `pub(crate) fn` can push a signature past `max_width`, and deletions
//! leave blank-line residue. A workspace whose CI runs `cargo fmt --check`
//! would then fail on the fixed tree. So `--fix` probes `cargo fmt --check`
//! BEFORE its first write (an `[expand]` rewrite included), and runs
//! `cargo fmt` afterwards only when the tree was clean: it then reformats
//! exactly the fix residue, never code the author left unformatted on
//! purpose. Otherwise (not clean, or `cargo fmt` unavailable) it prints the
//! hint to do it by hand.
//!
//! Plain `cargo fmt` from the workspace root formats every member — and,
//! unlike `--all`, never a local path dependency outside the workspace, which
//! `--fix` didn't touch and whose repository the clean-tree guard doesn't
//! cover. It is the `cargo` on `PATH`: the same command the author's own
//! `cargo fmt --check` (locally or in CI) runs.

use std::process::{Command, Stdio};

/// The pre-fix probe's result.
pub(crate) struct Tidy {
    clean_before: bool,
}

impl Tidy {
    /// Probe the workspace — call before `--fix` writes anything.
    pub(crate) fn probe() -> Self {
        Self {
            clean_before: cargo_fmt(true).is_ok(),
        }
    }

    /// A probe that saw an unclean tree: never reformats (unit tests of the
    /// fix engine must not spawn `cargo fmt`).
    #[cfg(test)]
    pub(crate) fn unclean() -> Self {
        Self {
            clean_before: false,
        }
    }

    /// After a fix pass: reformat if the tree was fmt-clean before (and
    /// something changed), else hint.
    pub(crate) fn after(&self, modified: bool) {
        if !modified {
            return;
        }
        if !self.clean_before {
            eprintln!("note: run `cargo fmt` to tidy the fixed files");
            return;
        }
        match cargo_fmt(false) {
            Ok(()) => eprintln!(
                "workspace-lint --fix: ran `cargo fmt` on the fixed workspace \
                 (it was fmt-clean before the fix)"
            ),
            // Clean before, failing now: the likeliest cause is a fix that
            // left code rustfmt can't parse — say so, with rustfmt's words.
            Err(stderr) => eprintln!(
                "warning: `cargo fmt` failed on the fixed workspace, which was fmt-clean \
                 before the fix — the fix may have left code rustfmt can't parse; review \
                 the fixed files:\n{}",
                stderr.trim_end()
            ),
        }
    }
}

/// `cargo fmt [--check]` in the workspace (the process cwd, its root);
/// `Err` carries rustfmt's stderr. The caller's toolchain selection is
/// scrubbed so the linted workspace's own `rust-toolchain.toml` (and its
/// rustfmt) applies, not the one this binary happened to be launched under.
fn cargo_fmt(check: bool) -> Result<(), String> {
    let mut cmd = Command::new("cargo");
    cmd.arg("fmt");
    if check {
        cmd.arg("--check");
    }
    let out = cmd
        .env_remove("RUSTUP_TOOLCHAIN")
        .env_remove("CARGO")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("could not run `cargo fmt`: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}
