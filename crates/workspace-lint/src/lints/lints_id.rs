//! Central registry of every lint name workspace-lint can emit.
//!
//! Used as a single source of truth by:
//! - [`messages::scenarios`](crate::messages::scenarios) — every variant in
//!   [`LintId::ALL`] must have at least one scenario for the human/json/github
//!   snapshot tests.
//! - The `#[cfg(test)] mod tests` below — `every_lint_has_a_message_scenario`
//!   and `every_fixturable_lint_has_a_fix_fixture` are the missing-test-files
//!   guards.
//! - `tests/fix_fixtures.rs` — the [`FIXTURABLE_LINTS`] subset must each have
//!   a paired `tests/fixtures/fix__<short>/` directory.
//! - The `[lints]` config table (see [`crate::config`]) — every key is a
//!   short name of a known [`LintId`].
//!
//! When you add a new lint:
//! 1. Add a [`LintId`] variant and wire its `id`/`short` arms.
//! 2. Include it in [`LintId::ALL`].
//! 3. Replace the per-module `pub const LINT: &str = ...` with a reference
//!    to [`LintId::<variant>::id()`].
//! 4. Add a scenario in [`crate::messages::scenarios`].
//! 5. Either add a `fix__<short>` fixture and put the variant in
//!    [`FIXTURABLE_LINTS`], or document why it's omitted in the comment
//!    block below.

/// Compile-time identity for every lint workspace-lint can emit.
///
/// The exhaustive match in [`Self::id`] makes adding a variant without
/// wiring its lint-ID string a compile error. The runtime registry tests in
/// this module check that [`Self::ALL`] also stays in sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum LintId {
    Architecture,
    CentralizedDeps,
    CliCrateVersion,
    Config,
    CrateSize,
    FeatureDrift,
    FileSize,
    Freshness,
    ModuleTree,
    StaleExpect,
    StaleGitIndex,
    UnknownLint,
    UnusedDeps,
    UnusedPub,
}

impl LintId {
    /// Every lint variant, in stable (alphabetical-by-id) order. Order is
    /// asserted by `tests::all_ids_are_sorted`.
    pub const ALL: &'static [LintId] = &[
        LintId::Architecture,
        LintId::CentralizedDeps,
        LintId::CliCrateVersion,
        LintId::Config,
        LintId::CrateSize,
        LintId::FeatureDrift,
        LintId::FileSize,
        LintId::Freshness,
        LintId::ModuleTree,
        LintId::StaleExpect,
        LintId::StaleGitIndex,
        LintId::UnknownLint,
        LintId::UnusedDeps,
        LintId::UnusedPub,
    ];

    /// The full `workspace-lint::<short>` identifier emitted in diagnostics
    /// and accepted by config / suppression directives.
    pub const fn id(self) -> &'static str {
        match self {
            Self::Architecture => "workspace-lint::architecture",
            Self::CentralizedDeps => "workspace-lint::centralized-deps",
            Self::CliCrateVersion => "workspace-lint::cli-crate-version",
            Self::Config => "workspace-lint::config",
            Self::CrateSize => "workspace-lint::crate-size",
            Self::FeatureDrift => "workspace-lint::feature-drift",
            Self::FileSize => "workspace-lint::file-size",
            Self::Freshness => "workspace-lint::freshness",
            Self::ModuleTree => "workspace-lint::module-tree",
            Self::StaleExpect => "workspace-lint::stale-expect",
            Self::StaleGitIndex => "workspace-lint::stale-git-index",
            Self::UnknownLint => "workspace-lint::unknown-lint",
            Self::UnusedDeps => "workspace-lint::unused-deps",
            Self::UnusedPub => "workspace-lint::unused-pub",
        }
    }

    /// Short kebab name (no `workspace-lint::` prefix). Used in fixture
    /// directory names, comment directives, and the `[lints]` config table.
    pub const fn short(self) -> &'static str {
        match self {
            Self::Architecture => "architecture",
            Self::CentralizedDeps => "centralized-deps",
            Self::CliCrateVersion => "cli-crate-version",
            Self::Config => "config",
            Self::CrateSize => "crate-size",
            Self::FeatureDrift => "feature-drift",
            Self::FileSize => "file-size",
            Self::Freshness => "freshness",
            Self::ModuleTree => "module-tree",
            Self::StaleExpect => "stale-expect",
            Self::StaleGitIndex => "stale-git-index",
            Self::UnknownLint => "unknown-lint",
            Self::UnusedDeps => "unused-deps",
            Self::UnusedPub => "unused-pub",
        }
    }

    /// Reverse of [`Self::short`]: look up a variant by its kebab name.
    /// Returns `None` for unknown names.
    pub fn from_short(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|v| v.short() == s)
    }

    /// `true` for the *policy* lints that have no meaning without parameters,
    /// so the presence of their config table is the opt-in: `file-size`,
    /// `crate-size`, `freshness`, `cli-crate-version`, `architecture`.
    ///
    /// The remaining (*structural*) lints catch universal mistakes with zero
    /// required config, so they run whenever their effective level isn't
    /// `allow`. This is the single source of truth for that distinction; see
    /// [`crate::config`] for how it drives enablement.
    pub const fn requires_config(self) -> bool {
        matches!(
            self,
            Self::FileSize
                | Self::CrateSize
                | Self::Freshness
                | Self::CliCrateVersion
                | Self::Architecture
        )
    }
}

/// Compatibility view: every lint's full ID in stable order. Equivalent to
/// `LintId::ALL.iter().map(|l| l.id())`. Test-only (registry invariants).
#[allow(dead_code)]
pub(crate) const ALL_LINTS: &[&str] = &[
    LintId::Architecture.id(),
    LintId::CentralizedDeps.id(),
    LintId::CliCrateVersion.id(),
    LintId::Config.id(),
    LintId::CrateSize.id(),
    LintId::FeatureDrift.id(),
    LintId::FileSize.id(),
    LintId::Freshness.id(),
    LintId::ModuleTree.id(),
    LintId::StaleExpect.id(),
    LintId::StaleGitIndex.id(),
    LintId::UnknownLint.id(),
    LintId::UnusedDeps.id(),
    LintId::UnusedPub.id(),
];

/// Lints with a paired `tests/fixtures/fix__<short>/` directory exercised
/// by `tests/fix_fixtures.rs`. Only lints that emit a real structural
/// rewrite (byte-range replacement with `Applicability::MachineApplicable`)
/// belong here — `--fix` never inserts silence directives, so any lint
/// without a structural fix has nothing for the fixture driver to assert
/// on.
///
/// Omitted today:
/// - `file-size`, `crate-size`: no structural fix — heuristic refactoring
///   isn't tractable. `--fix` is a no-op; users must split files manually.
/// - `freshness`: needs mtime manipulation that can't live inert in a
///   committed fixture (timestamps move on every clone / checkout).
/// - `cli-crate-version`: needs a fake CLI binary the fixture can invoke.
/// - `stale-expect`: depends on a prior `expect!` directive matching a
///   diagnostic — the test would be testing the suppression pipeline, not
///   `--fix` mechanics.
/// - `stale-git-index`: needs `git ls-files` to disagree with on-disk
///   state, which requires an in-tempdir git init/add/rm dance.
/// - `config`, `unknown-lint`: config-validation diagnostics with no
///   structural fix — the remedy is a hand edit of the config / directive.
/// - `architecture`, `feature-drift`, `module-tree`: the structural fixes
///   for these are planned but not yet implemented; once `--fix` rewrites
///   them, add fixtures and move them up.
#[allow(dead_code)] // test-only (drives the fix-fixture coverage guard)
pub(crate) const FIXTURABLE_LINTS: &[&str] = &[
    LintId::CentralizedDeps.id(),
    LintId::UnusedDeps.id(),
    LintId::UnusedPub.id(),
];

/// Strip the `workspace-lint::` prefix so callers can derive the short
/// kebab form (`file-size`) used in fixture directory names and comment
/// directives. Test-only (the fix-fixture coverage guard).
#[allow(dead_code)]
pub(crate) fn short(lint: &str) -> &str {
    lint.strip_prefix("workspace-lint::").unwrap_or(lint)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- registry invariants ---

    #[test]
    fn all_ids_are_sorted() {
        // Stable order is a property the coverage tests rely on for clear
        // error messages.
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
    fn lintid_all_matches_all_lints() {
        let from_enum: Vec<&str> = LintId::ALL.iter().map(|l| l.id()).collect();
        assert_eq!(from_enum, ALL_LINTS);
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

    #[test]
    fn lintid_short_round_trips() {
        for &id in LintId::ALL {
            assert_eq!(LintId::from_short(id.short()), Some(id));
            // Short name also matches what `short()` would return on the id.
            assert_eq!(short(id.id()), id.short());
        }
    }

    // --- missing-test-files guard ---

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

    // --- registry coverage: every non-meta lint must actually run ---

    /// A maximal config that turns on every lint: each *policy* lint
    /// ([`LintId::requires_config`]) needs its config table present, and no
    /// lint is `allow`-ed (the built-in default is `warn`). Used to assert the
    /// registry builds an instance for every non-meta lint.
    const ALL_LINTS_ENABLED_CONFIG: &str = "\
[[file-size.rules]]
glob = \"**/*.rs\"
max-code-lines = 500

[[crate-size.rules]]
glob = \"crates/*\"
max-code-lines = 5000

[[freshness.rules]]
glob = \"**/CLAUDE.md\"
depends-on = \"**/*.rs\"

[[cli-crate-version.rules]]
command = [\"wasm-bindgen\", \"--version\"]
pattern = \"wasm-bindgen (\\\\S+)\"
crate = \"wasm-bindgen\"

[[architecture.rules]]
from = [\"crates/a/**\"]
deny = [\"crates/b/**\"]
";

    /// Lints that are emitted by the config-audit / suppression pipeline rather
    /// than a [`crate::lints::registry`] entry — they have no `Lint` instance.
    /// Every *other* variant must be produced by the registry.
    const META_LINTS: &[LintId] = &[LintId::Config, LintId::StaleExpect, LintId::UnknownLint];

    #[test]
    fn registry_covers_every_non_meta_lint() {
        let config: crate::config::Config =
            toml::from_str(ALL_LINTS_ENABLED_CONFIG).expect("maximal config parses");
        let produced: std::collections::HashSet<LintId> = crate::lints::registry(&config)
            .iter()
            .map(|l| l.id())
            .collect();

        let mut missing: Vec<&str> = LintId::ALL
            .iter()
            .filter(|id| !META_LINTS.contains(id) && !produced.contains(id))
            .map(|id| id.short())
            .collect();
        missing.sort();
        assert!(
            missing.is_empty(),
            "every non-meta lint in LintId::ALL must be built by registry(); missing: {missing:?}\n\
             Add a line in `lints::registry` wiring the new lint to its config block \
             (or, if it's a config-audit/suppression meta lint, add it to META_LINTS)."
        );

        for id in META_LINTS {
            assert!(
                !produced.contains(id),
                "meta lint `{}` must not appear in registry()",
                id.short()
            );
        }
    }

    // --- marker-crate drift: the `expect!`/`allow!` macro arms must track
    //     LintId so a stale arm (e.g. a removed lint) or a missing one fails ---

    /// The two lints that are anchored to the config *file*, not a `.rs` source
    /// item — a marker macro (which only appears in Rust source) can't silence
    /// them, so the marker crate deliberately omits them.
    const CONFIG_FILE_ONLY: &[LintId] = &[LintId::Config, LintId::UnknownLint];

    /// Parse the marker crate's source for the single-lint macro arms (lines of
    /// the form `(some_ident) => {};`), returning their kebab-cased names. The
    /// recursive comma-list arm (`($first:ident, …)`) is skipped because its
    /// inner text isn't a bare ident.
    fn marker_arm_lints() -> std::collections::HashSet<String> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../workspace-lint-marker/src/lib.rs");
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let mut out = std::collections::HashSet::new();
        for line in src.lines() {
            let t = line.trim();
            let Some(rest) = t.strip_prefix('(') else {
                continue;
            };
            let Some((inner, tail)) = rest.split_once(')') else {
                continue;
            };
            if !tail.trim_start().starts_with("=>") {
                continue;
            }
            // A single bare ident — skips the `$first:ident, …` recursive arm.
            if !inner.is_empty()
                && inner
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit())
            {
                out.insert(inner.replace('_', "-"));
            }
        }
        out
    }

    #[test]
    fn marker_macro_arms_match_lint_ids() {
        let arms = marker_arm_lints();
        let expected: std::collections::HashSet<String> = LintId::ALL
            .iter()
            .filter(|id| !CONFIG_FILE_ONLY.contains(id))
            .map(|id| id.short().to_string())
            .collect();

        let mut missing: Vec<&String> = expected.difference(&arms).collect();
        missing.sort();
        assert!(
            missing.is_empty(),
            "workspace-lint-marker is missing `expect!`/`allow!` arms for: {missing:?}\n\
             Add the arm in crates/workspace-lint-marker/src/lib.rs (snake_case)."
        );

        let mut stale: Vec<&String> = arms.difference(&expected).collect();
        stale.sort();
        assert!(
            stale.is_empty(),
            "workspace-lint-marker has macro arms for non-existent lints: {stale:?}\n\
             Remove the stale arm, or add the lint to LintId (or CONFIG_FILE_ONLY)."
        );
    }
}
