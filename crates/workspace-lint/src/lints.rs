//! Central registry of every lint name workspace-lint can emit.
//!
//! Used as a single source of truth by:
//! - [`messages::scenarios`](crate::messages::scenarios) — every lint must
//!   have at least one scenario for the human/json/github snapshot tests.
//! - `tests/lint_coverage.rs` — the missing-test-files guard.
//! - `tests/fix_fixtures.rs` — the `FIXTURABLE_LINTS` subset must each have
//!   a paired `tests/fixtures/fix__<short>/` directory.
//!
//! When you add a new lint:
//! 1. Add its `pub const LINT: &str` next to the check.
//! 2. Reference it here in `ALL_LINTS`.
//! 3. Add a scenario in [`crate::messages::scenarios`].
//! 4. Either add a `fix__<short>` fixture and put the lint in
//!    [`FIXTURABLE_LINTS`], or document why it's omitted in the comment
//!    block below.

pub const ALL_LINTS: &[&str] = &[
    crate::centralized_deps::LINT,
    crate::cli_crate_version::LINT,
    crate::crate_size::LINT,
    crate::file_size::LINT,
    crate::freshness::LINT,
    crate::suppress::STALE_EXPECT_LINT,
    crate::file_size::STALE_GIT_INDEX_LINT,
    crate::unused_deps::LINT,
    crate::unused_pub::LINT,
];

/// Lints with a paired `tests/fixtures/fix__<short>/` directory exercised
/// by `tests/fix_fixtures.rs`. The rest are documented inline below — if
/// you add scaffolding that lets one of them be fixture-tested cleanly,
/// move it up here.
///
/// Omitted today:
/// - `freshness`: needs mtime manipulation that can't live inert in a
///   committed fixture (timestamps move on every clone / checkout).
/// - `cli-crate-version`: needs a fake CLI binary the fixture can invoke.
/// - `unused-pub`: needs a pre-generated SCIP index per fixture (running
///   rust-analyzer in tests is slow and brittle).
/// - `stale-expect`: depends on a prior `expect!` directive matching a
///   diagnostic — the test would be testing the suppression pipeline, not
///   `--fix` mechanics.
/// - `stale-git-index`: needs `git ls-files` to disagree with on-disk
///   state, which requires an in-tempdir git init/add/rm dance.
pub const FIXTURABLE_LINTS: &[&str] = &[
    crate::centralized_deps::LINT,
    crate::crate_size::LINT,
    crate::file_size::LINT,
    crate::unused_deps::LINT,
];

/// Strip the `workspace-lint::` prefix so callers can derive the short
/// kebab form (`file-size`) used in fixture directory names and comment
/// directives.
pub fn short(lint: &str) -> &str {
    lint.strip_prefix("workspace-lint::").unwrap_or(lint)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- registry invariants ---

    #[test]
    fn all_lints_is_sorted() {
        // Stable order is a property the coverage tests rely on for clear
        // error messages. Keep this list alphabetized.
        let mut sorted = ALL_LINTS.to_vec();
        sorted.sort();
        assert_eq!(sorted, ALL_LINTS);
    }

    #[test]
    fn all_lints_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for lint in ALL_LINTS {
            assert!(seen.insert(*lint), "duplicate lint in ALL_LINTS: {lint}");
        }
    }

    #[test]
    fn every_lint_uses_workspace_lint_prefix() {
        for lint in ALL_LINTS {
            assert!(
                lint.starts_with("workspace-lint::"),
                "lint `{lint}` does not start with `workspace-lint::`"
            );
        }
    }

    #[test]
    fn fixturable_is_subset_of_all() {
        for lint in FIXTURABLE_LINTS {
            assert!(
                ALL_LINTS.contains(lint),
                "FIXTURABLE_LINTS contains `{lint}` which is not in ALL_LINTS"
            );
        }
    }

    #[test]
    fn short_strips_prefix() {
        assert_eq!(short("workspace-lint::file-size"), "file-size");
        assert_eq!(short("no-prefix"), "no-prefix");
    }

    // --- missing-test-files guard (lesson 3 from clippy) ---

    #[test]
    fn every_lint_has_a_message_scenario() {
        let scenarios = crate::messages::scenarios();
        let covered: std::collections::HashSet<&str> =
            scenarios.iter().map(|(_, d)| d.lint.as_ref()).collect();
        let mut missing: Vec<&str> = ALL_LINTS
            .iter()
            .filter(|lint| !covered.contains(*lint))
            .copied()
            .collect();
        missing.sort();
        assert!(
            missing.is_empty(),
            "every lint in ALL_LINTS must appear in messages::scenarios(); missing: {missing:?}\n\
             Add a scenario at the bottom of `scenarios()` in src/messages.rs and \
             corresponding snapshot tests."
        );
    }

    #[test]
    fn every_fixturable_lint_has_a_fix_fixture() {
        let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let fixtures = manifest.join("tests/fixtures");
        let mut missing: Vec<String> = Vec::new();
        for lint in FIXTURABLE_LINTS {
            let name = short(lint).replace('-', "_");
            let dir = fixtures.join(format!("fix__{name}"));
            let input = dir.join("input");
            let expected = dir.join("expected");
            if !input.is_dir() || !expected.is_dir() {
                missing.push(format!(
                    "fix__{name} (missing input/ or expected/ for lint `{lint}`)"
                ));
            }
        }
        assert!(
            missing.is_empty(),
            "every lint in FIXTURABLE_LINTS must have a paired fixture under \
             tests/fixtures/fix__<name>/{{input,expected}}/; missing: {missing:#?}\n\
             Add the fixture, then a #[test] wrapper in tests/fix_fixtures.rs."
        );
    }
}
