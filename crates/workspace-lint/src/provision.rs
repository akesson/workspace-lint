//! Interactive remediation for engine preflight failures.
//!
//! The engine layer stays non-interactive: [`wl_engine::EngineError`] *names*
//! its repair (`remediation()`), and this module — CLI-side, terminal-gated —
//! offers to run it so a first full-tier run is one keypress instead of a
//! copy-paste round trip. It never prompts without a terminal on both stdin
//! and stderr (CI and pipes keep the plain error), never runs anything the
//! error text didn't show verbatim (a lockstep test in `wl-engine` ties the
//! two), and never re-runs a command that already ran once — a repair that
//! didn't repair must fail loudly, not loop.

use std::collections::HashSet;
use std::io::{BufRead, IsTerminal, Write};

use wl_engine::EngineError;
use wl_lints::util;

/// One extraction attempt's provisioning state: which repairs already ran.
pub(crate) struct Provisioner {
    attempted: HashSet<Vec<String>>,
}

impl Provisioner {
    pub(crate) fn new() -> Self {
        Self {
            attempted: HashSet::new(),
        }
    }

    /// Repair `err` with the user's consent, or exit the process. Returning
    /// at all means the remediation command succeeded and the caller should
    /// retry the operation that failed; every other path diverges through
    /// [`util::fail`] (exit 2) so no caller can misread a decline as success.
    pub(crate) fn repair_or_fail(&mut self, err: &EngineError) {
        let Some(argv) = err.remediation() else {
            util::fail(err);
        };
        // A command that already ran and still yields the same failure is a
        // broken environment, not a prompting opportunity.
        if !self.attempted.insert(argv.clone()) {
            util::fail(err);
        }
        if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
            util::fail(err);
        }
        eprintln!("{err}\n");
        eprint!("workspace-lint can run that for you — proceed? [y/N] ");
        let _ = std::io::stderr().flush();
        let mut answer = String::new();
        let _ = std::io::stdin().lock().read_line(&mut answer);
        if !accepted(&answer) {
            // The error and its paste-able command are already on screen.
            std::process::exit(2);
        }
        run(&argv);
    }
}

/// Only an explicit yes provisions; enter, EOF, or anything else declines.
fn accepted(answer: &str) -> bool {
    matches!(answer.trim(), "y" | "Y" | "yes" | "Yes" | "YES")
}

/// Run one remediation command with inherited stdio — rustup/cargo progress
/// is the user's feedback that consent did something. Diverges on failure.
fn run(argv: &[String]) {
    eprintln!();
    let status = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .status();
    match status {
        Ok(s) if s.success() => eprintln!(),
        Ok(s) => util::fail(format!("`{}` failed ({s})", argv.join(" "))),
        Err(e) => util::fail(format!("spawning `{}`: {e}", argv.join(" "))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_explicit_yes_accepts() {
        for yes in ["y\n", "Y\n", "yes\n", " yes \n", "YES"] {
            assert!(accepted(yes), "{yes:?} should accept");
        }
        for no in ["\n", "", "n\n", "no\n", "q\n", "j\n", "yess\n"] {
            assert!(!accepted(no), "{no:?} must decline");
        }
    }

    /// The dedup key is the argv itself: the same failure twice is refused,
    /// while a *different* repair (the next preflight stage) still offers.
    #[test]
    fn each_remediation_is_attempted_at_most_once() {
        let mut p = Provisioner::new();
        let toolchain = EngineError::ToolchainMissing {
            pin: "nightly-2026-04-16".into(),
        }
        .remediation()
        .unwrap();
        let link = EngineError::DylintLinkMissing.remediation().unwrap();
        assert!(p.attempted.insert(toolchain.clone()));
        assert!(!p.attempted.insert(toolchain));
        assert!(p.attempted.insert(link));
    }
}
