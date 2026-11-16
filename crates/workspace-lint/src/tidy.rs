//! Keep a fmt-clean workspace fmt-clean across `--fix`.
//!
//! A fix is a byte-range rewrite with no idea of layout: `pub fn` →
//! `pub(crate) fn` can push a signature past `max_width`, and deletions
//! leave blank-line residue. A workspace whose CI runs `cargo fmt --check`
//! would then fail on the fixed tree. So `--fix` probes `cargo fmt --check`
//! BEFORE writing anything, and runs `cargo fmt` afterwards only when the
//! tree was clean: it then reformats exactly the fix residue, never code the
//! author left unformatted on purpose. Otherwise (not clean, or `cargo fmt`
//! unavailable) it prints the hint to do it by hand.

use std::process::{Command, Stdio};

/// The pre-fix probe's result.
pub(crate) struct Tidy {
    clean_before: bool,
}

impl Tidy {
    /// Probe the workspace before any fix is written.
    pub(crate) fn probe() -> Self {
        Self {
            clean_before: cargo_fmt(true),
        }
    }

    /// After a fix pass: reformat if the tree was fmt-clean before (and
    /// something changed), else hint.
    pub(crate) fn after(&self, modified: bool) {
        if !modified {
            return;
        }
        if self.clean_before && cargo_fmt(false) {
            eprintln!(
                "workspace-lint --fix: ran `cargo fmt` on the fixed workspace \
                 (it was fmt-clean before the fix)"
            );
        } else {
            eprintln!("note: run `cargo fmt` to tidy the fixed files");
        }
    }
}

/// `cargo fmt --all [--check]` in the workspace (the process cwd), silent;
/// `true` iff it ran and succeeded. The caller's toolchain selection is
/// scrubbed so the linted workspace's own `rust-toolchain.toml` (and its
/// rustfmt) applies, not the one this binary happened to be launched under.
fn cargo_fmt(check: bool) -> bool {
    let mut cmd = Command::new("cargo");
    cmd.args(["fmt", "--all"]);
    if check {
        cmd.arg("--check");
    }
    cmd.env_remove("RUSTUP_TOOLCHAIN")
        .env_remove("CARGO")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}
