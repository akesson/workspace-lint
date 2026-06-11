//! Deep `--fix` verification: run `rust-analyzer scip`, ingest its index, and
//! use it as a one-directional oracle over the reference-evidence findings
//! (`unused-deps`, `unused-pub`) before `--fix` acts on them.
//!
//! The doctrine (`DESIGN-ir-pipeline.md` §10): SCIP is ground truth for "is
//! crate X referenced" (it sees through method calls and macro expansion that
//! the syn-based resolver can't), so it can only ever **disprove** one of our
//! findings — confirm the resolver and rust-analyzer agree (apply the
//! structural fix) or catch a resolver false positive (write a suppression
//! directive instead, gated on the clean tree the `--fix` entry requires).
//! It never creates a finding and never upgrades a `MaybeIncorrect` suggestion.
//!
//! Submodules:
//! - [`normalize`] — project a SCIP symbol onto canonical segments (§8).
//! - [`index`] — load + flatten a `rust-analyzer scip` index.
//! - [`verify`] — match findings against the index, mutate the disproved ones.
//! - [`directive`] — build the `expect` insertion written for a disproof.

mod directive;
pub(crate) mod index;
pub(crate) mod normalize;
mod verify;

use std::path::{Path, PathBuf};
use std::process::Command;

use syn_workspace::Workspace;

use crate::diagnostic::{Applicability, Diagnostic, Suggestion};
use index::ScipIndex;

/// Run deep verification over the findings and return the directive insertions
/// `fix::run` should apply alongside the surviving structural fixes. Mutates
/// `diagnostics` in place (downgrading disproved suggestions, annotating their
/// findings) and prints a one-line summary.
///
/// Short-circuits to an empty result — never invoking rust-analyzer — when
/// there's no workspace or no `MachineApplicable` evidence-bearing finding to
/// check. When there *is* work and `scip_index` is `None`, it runs
/// `rust-analyzer scip`; a missing binary or a failed/empty index is a hard
/// error (the whole point is to avoid acting on resolver-only evidence, so
/// silently degrading would defeat it — pass `--no-deep` to opt out instead).
pub(crate) fn verify_findings(
    diagnostics: &mut [Diagnostic],
    workspace: Option<&Workspace>,
    scip_index: Option<&Path>,
) -> Vec<Suggestion> {
    let Some(workspace) = workspace else {
        return Vec::new(); // no resolver model ⇒ no reference-evidence findings
    };
    if !has_verifiable_finding(diagnostics) {
        return Vec::new(); // nothing to check ⇒ don't pay for rust-analyzer
    }

    let index_path = match scip_index {
        Some(p) => p.to_path_buf(),
        None => run_rust_analyzer(workspace.root()),
    };
    let index = ScipIndex::load(&index_path).unwrap_or_else(|e| {
        eprintln!("error: deep verification could not read the SCIP index: {e}");
        eprintln!("hint: pass --no-deep to skip rust-analyzer verification.");
        std::process::exit(2);
    });

    let outcome = verify::verify(diagnostics, &index, workspace);
    eprintln!(
        "workspace-lint --fix: deep verification — {} confirmed, {} disproved ({} expect directive{} queued)",
        outcome.confirmed,
        outcome.disproved,
        outcome.inserts.len(),
        if outcome.inserts.len() == 1 { "" } else { "s" },
    );
    outcome.inserts
}

/// `true` if any diagnostic carries a `MachineApplicable` suggestion with
/// deep-verification evidence — the only kind worth invoking rust-analyzer for.
fn has_verifiable_finding(diagnostics: &[Diagnostic]) -> bool {
    diagnostics.iter().any(|d| {
        d.suggestions
            .iter()
            .any(|s| s.applicability == Applicability::MachineApplicable && s.evidence.is_some())
    })
}

/// Invoke `rust-analyzer scip` over `root`, writing the index under
/// `target/workspace-lint/` (gitignored). Hard-exits with an install hint on a
/// missing binary or a non-zero exit.
fn run_rust_analyzer(root: &Path) -> PathBuf {
    let out_dir = root.join("target").join("workspace-lint");
    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        eprintln!("error: could not create {}: {e}", out_dir.display());
        std::process::exit(2);
    }
    let out = out_dir.join("index.scip");
    eprintln!("workspace-lint --fix: running `rust-analyzer scip` for deep verification…");
    let status = Command::new("rust-analyzer")
        .arg("scip")
        .arg(root)
        .arg("--output")
        .arg(&out)
        .status();
    match status {
        Err(e) => {
            eprintln!("error: could not run `rust-analyzer scip`: {e}");
            eprintln!(
                "hint: install it (`rustup component add rust-analyzer`), pass an existing \
                 index with --scip-index <path>, or skip deep verification with --no-deep."
            );
            std::process::exit(2);
        }
        Ok(s) if !s.success() => {
            eprintln!("error: `rust-analyzer scip` failed (exit {:?})", s.code());
            eprintln!("hint: pass --no-deep to skip deep verification.");
            std::process::exit(2);
        }
        Ok(_) => out,
    }
}
