//! Small cross-cutting helpers shared by more than one module.

/// Print `msg` to stderr and exit with the **operational-error** code `2`.
///
/// Exit-code policy (the single source of truth for the binary):
/// - `0` — clean: no surviving findings.
/// - `1` — lint findings survived (a `Deny`-level diagnostic). Set only by
///   `report_and_exit`.
/// - `2` — operational error: unusable config, a failed subprocess, an IO
///   error, a dirty tree under `--fix`, etc. Routed through this helper so the
///   two codes never get confused (`1` must mean "the code under test has lint
///   findings", not "the tool itself broke").
pub(crate) fn fail(msg: impl std::fmt::Display) -> ! {
    eprintln!("{msg}");
    std::process::exit(2);
}

/// A crate name with all `-`/`_` separators removed, for the FP-safe lib-name
/// fallback match (`md_5` collapses to `md5` to match a `md5` lib target).
///
/// Load-bearing in lockstep: `unused-deps` uses it to decide a dependency is
/// unused, and `deep::verify` uses the *same* normalization to decide whether
/// SCIP disproves that finding. If the two diverged, `--fix` could delete a dep
/// the verifier would have vouched for — so they share this single definition.
pub(crate) fn separator_stripped(name: &str) -> String {
    name.chars().filter(|c| *c != '-' && *c != '_').collect()
}
