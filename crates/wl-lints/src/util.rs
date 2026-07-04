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
pub fn fail(msg: impl std::fmt::Display) -> ! {
    eprintln!("{msg}");
    std::process::exit(2);
}

/// A crate name with all `-`/`_` separators removed, for the FP-safe lib-name
/// fallback match (`md_5` collapses to `md5` to match a `md5` lib target).
pub(crate) fn separator_stripped(name: &str) -> String {
    name.chars().filter(|c| *c != '-' && *c != '_').collect()
}

/// Split a CLI `--command` string into argv using shell-like quoting, so
/// `--command "tool --flag 'a b'"` survives args with spaces (the old naive
/// whitespace split mangled them). Exits with a clear message on unbalanced
/// quotes. Shared by the `cli-crate-version` lint and the binary's `check`
/// subcommand.
pub fn split_command(command: &str) -> Vec<String> {
    shell_words::split(command).unwrap_or_else(|e| {
        eprintln!("error: could not parse --command `{command}`: {e}");
        std::process::exit(2);
    })
}
