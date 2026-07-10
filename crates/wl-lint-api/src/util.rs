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
pub fn separator_stripped(name: &str) -> String {
    name.chars().filter(|c| *c != '-' && *c != '_').collect()
}

/// Walk `root` (`.gitignore`-aware, via the `ignore` crate) and collect every
/// file whose root-relative path matches `matcher`. Used by the binary's
/// `expand` scanner.
pub fn walk_files_matching(
    root: &std::path::Path,
    matcher: &globset::GlobMatcher,
) -> Vec<std::path::PathBuf> {
    let mut results = Vec::new();
    for entry in ignore::WalkBuilder::new(root).build().flatten() {
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let rel = entry.path().strip_prefix(root).unwrap_or(entry.path());
        if matcher.is_match(rel) {
            results.push(entry.into_path());
        }
    }
    results
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

/// The `configured by [[<lint>.rules]] glob = …` attribution note the size
/// lints stamp once per rule (via `note_once`) — one wording, one format
/// string, instead of a per-lint copy.
pub fn rule_glob_note(lint: crate::LintId, glob: &crate::config::GlobPattern) -> String {
    format!(
        "configured by [[{}.rules]] glob = \"{}\"",
        lint.short(),
        glob.as_str()
    )
}

/// Start a crate-anchored diagnostic for a manifest-level finding.
/// Workspace-relative paths everywhere — both the in-message path handed to
/// `message` and the suppression anchor — so a per-Cargo.toml
/// `# workspace-lint: allow(...)` directive can actually match the
/// diagnostic's `SilenceAnchor::Crate`. `display_path` forces forward slashes
/// so Windows runs produce the same diagnostic text as Linux/macOS (snapshot
/// fixtures lock it in). Shared by `centralized-deps` and `unused-deps` —
/// extracted when `duplicate-code`'s statement-run upgrade flagged the two
/// hand-rolled copies as clones.
pub fn at_crate_manifest(
    lint_id: &'static str,
    fast: &wl_engine::fast::FastModel,
    manifest_dir: &std::path::Path,
    manifest_path: &std::path::Path,
    message: impl FnOnce(&str) -> String,
) -> wl_diagnostic::builder::DiagnosticBuilder {
    let manifest_dir_rel = fast.crate_relative_path(manifest_dir);
    let manifest_path_rel = fast.crate_relative_path(manifest_path);
    let cargo_path_str = wl_diagnostic::render::display_path(&manifest_path_rel);
    wl_diagnostic::builder::at_crate(lint_id, message(&cargo_path_str), manifest_dir_rel)
}
