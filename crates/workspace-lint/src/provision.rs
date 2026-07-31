//! Provisioning the full tier's requirements — two entry points over the
//! same engine-named repairs.
//!
//! The engine layer stays non-interactive: [`wl_engine::EngineError`] *names*
//! its repair (`remediation()`), and this module runs those commands in one
//! of two modes. [`Provisioner`] is the interactive path — terminal-gated, it
//! offers each repair as a one-keypress install-and-retry when a default run
//! trips preflight. It never prompts without a terminal on both stdin and
//! stderr (CI and pipes keep the plain error), never runs anything the error
//! text didn't show verbatim (a lockstep test in `wl-engine` ties the two),
//! and never re-runs a command that already ran once — a repair that didn't
//! repair must fail loudly, not loop. [`run`] is the `provision` subcommand —
//! the non-interactive CI path over [`wl_engine::Engine::provision_plan`],
//! where invoking the subcommand is itself the consent.

use std::collections::HashSet;
use std::io::{BufRead, IsTerminal, Write};

use wl_engine::EngineError;
use wl_lint_api::util;

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

    /// Repair `err` with the user's consent. [`Repair::Retry`] means the
    /// remediation command succeeded and the caller should retry the
    /// operation that failed; [`Repair::GiveUp`] means the error stands —
    /// the caller decides what still renders before exiting (the fast-tier
    /// findings must not be swallowed by an engine failure), consulting
    /// `error_shown` to avoid printing the error twice.
    pub(crate) fn repair(&mut self, err: &EngineError) -> Repair {
        let unshown = Repair::GiveUp { error_shown: false };
        let Some(argv) = err.remediation() else {
            return unshown;
        };
        // A command that already ran and still yields the same failure is a
        // broken environment, not a prompting opportunity.
        if !self.attempted.insert(argv.clone()) {
            return unshown;
        }
        if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
            return unshown;
        }
        eprintln!("{err}\n");
        eprint!("workspace-lint can run that for you — proceed? [y/N] ");
        let _ = std::io::stderr().flush();
        let mut answer = String::new();
        let _ = std::io::stdin().lock().read_line(&mut answer);
        if !accepted(&answer) {
            // The error and its paste-able command are already on screen.
            return Repair::GiveUp { error_shown: true };
        }
        execute(&argv);
        Repair::Retry
    }
}

/// The `provision` subcommand: compute the state-aware plan (missing
/// toolchain / components / `--target` stds / `dylint-link`) from the pin
/// baked into this binary plus the `[engine] configs` matrix, then either
/// print it (`--print`: one command per line on stdout, for CI to audit or
/// pipe to `sh`) or run it with inherited stdio. The plan lists only what
/// this machine actually lacks, so the subcommand is idempotent — CI runs it
/// unconditionally and the toolchain pin never leaks into pipeline config.
pub(crate) fn run(print: bool, configs: &[wl_engine::ConfigSpec]) -> ! {
    let engine = wl_engine::Engine::new(wl_engine::ExtractorSource::vendored());
    let plan = plan_or_fail(&engine, configs);
    if print {
        print!("{}", render_plan(&plan));
    } else {
        apply(&engine, configs, &plan);
    }
    std::process::exit(0);
}

fn plan_or_fail(engine: &wl_engine::Engine, configs: &[wl_engine::ConfigSpec]) -> Vec<Vec<String>> {
    engine
        .provision_plan(configs)
        .unwrap_or_else(|e| util::fail(e.to_string()))
}

/// The `--print` surface: one shell-ready command per line, empty when the
/// machine is fully provisioned — so `provision --print | sh` and a human
/// audit read the same text.
fn render_plan(plan: &[Vec<String>]) -> String {
    plan.iter()
        .map(|argv| argv.join(" ") + "\n")
        .collect::<String>()
}

/// Run the plan with inherited stdio, then re-observe: a command can exit 0
/// yet not close its gap (a rustup shim resolving to an unexpected home, a
/// PATH that misses `~/.cargo/bin`), so success is reported only when the
/// preflight would now pass. Diverges (via [`execute`]/`util::fail`) on any
/// failure.
fn apply(engine: &wl_engine::Engine, configs: &[wl_engine::ConfigSpec], plan: &[Vec<String>]) {
    if plan.is_empty() {
        eprintln!("workspace-lint: the full tier is already provisioned");
        return;
    }
    for argv in plan {
        eprintln!("workspace-lint: running `{}`", argv.join(" "));
        execute(argv);
    }
    let unresolved = plan_or_fail(engine, configs);
    if !unresolved.is_empty() {
        util::fail(format!(
            "provisioning commands ran, but the full tier still lacks:\n{}",
            render_plan(&unresolved)
        ));
    }
    eprintln!("workspace-lint: full tier provisioned");
}

/// Outcome of one [`Provisioner::repair`] round.
pub(crate) enum Repair {
    /// The remediation ran successfully — retry the failed operation.
    Retry,
    /// No repair happened; the error stands. `error_shown` is `true` when
    /// the interactive prompt already printed it verbatim.
    GiveUp { error_shown: bool },
}

/// Only an explicit yes provisions; enter, EOF, or anything else declines.
fn accepted(answer: &str) -> bool {
    matches!(answer.trim(), "y" | "Y" | "yes" | "Yes" | "YES")
}

/// Run one remediation command with inherited stdio — rustup/cargo progress
/// is the user's feedback that consent did something. Diverges on failure.
fn execute(argv: &[String]) {
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

    /// `provision --print` output is shell input: one command per line,
    /// trailing newline, and *nothing at all* (not a blank line) when the
    /// machine is already provisioned.
    #[test]
    fn render_plan_is_one_shell_command_per_line() {
        assert_eq!(render_plan(&[]), "");
        let plan = vec![
            EngineError::ToolchainMissing {
                pin: "nightly-2026-04-16".into(),
            }
            .remediation()
            .unwrap(),
            EngineError::DylintLinkMissing.remediation().unwrap(),
        ];
        assert_eq!(
            render_plan(&plan),
            "rustup toolchain install nightly-2026-04-16 --profile minimal \
             --component rustc-dev --component llvm-tools-preview\n\
             cargo install dylint-link --locked\n"
        );
    }
}
