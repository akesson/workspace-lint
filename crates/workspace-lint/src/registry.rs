//! The composition root: build the runtime set of enabled lints from a loaded
//! [`Config`]. This is the one place lint *implementations* (`wl-lints`) meet
//! config *loading* (this binary) — hence it lives here, not in `wl-lints`.

use crate::config::{Config, LintLevel};
use wl_lint_api::{Lint, LintId};
use wl_lints::{
    architecture::Architecture, centralized_deps::CentralizedDeps,
    cli_crate_version::CliCrateVersion, crate_size::CrateSize, duplicate_code::DuplicateCode,
    feature_drift::FeatureDrift, file_size::FileSize, module_tree::ModuleTree,
    stale_git_index::StaleGitIndex, unused_deps::UnusedDeps, unused_pub::UnusedPub,
};

/// `true` when `id` is enabled *anywhere* — its global effective level isn't
/// `allow`, **or** some per-crate block turns it back on. A lint runs once for
/// the whole workspace and `apply_lint_levels` later drops the diagnostics that
/// land in crates where the effective level is `allow`, so "on for one crate"
/// means the lint must run. This is half the enable rule; *policy* lints
/// additionally require their config table to be present (checked inline below).
fn level_on(config: &Config, id: LintId) -> bool {
    if config.lints.effective(id) != LintLevel::Allow {
        return true;
    }
    config
        .crates
        .keys()
        .any(|name| config.effective_level(id, Some(name)) != LintLevel::Allow)
}

/// Build the runtime registry of enabled lints from the user's configuration.
///
/// Enablement is uniform: a lint runs iff its effective level isn't `allow`
/// **and** — for [`LintId::requires_config`] (policy) lints — its config
/// table is present. Structural lints need no table, so they're on by default.
/// Adding a lint is one new block here plus one folder under `crates/wl-lints/src/`.
pub(crate) fn registry(config: &Config) -> Vec<Box<dyn Lint>> {
    let mut out: Vec<Box<dyn Lint>> = Vec::new();

    // --- policy lints: gated on `level != allow` AND a present config table ---
    if level_on(config, LintId::Architecture)
        && let Some(ref ac) = config.architecture
        && ac.is_active()
    {
        out.push(Box::new(Architecture::new(ac.clone())));
    }
    if level_on(config, LintId::CliCrateVersion)
        && let Some(ref cv) = config.cli_crate_version
    {
        out.push(Box::new(CliCrateVersion::new(cv.clone())));
    }
    if level_on(config, LintId::CrateSize)
        && let Some(ref cs) = config.crate_size
    {
        out.push(Box::new(CrateSize::new(cs.clone())));
    }
    if level_on(config, LintId::DuplicateCode)
        && let Some(ref dc) = config.duplicate_code
    {
        out.push(Box::new(DuplicateCode::new(dc.clone())));
    }
    if level_on(config, LintId::FileSize)
        && let Some(ref fs) = config.file_size
    {
        out.push(Box::new(FileSize::new(fs.clone())));
    }

    // --- structural lints: on by default (no required config) ---
    if level_on(config, LintId::CentralizedDeps) {
        out.push(Box::new(CentralizedDeps::new()));
    }
    if level_on(config, LintId::FeatureDrift) {
        out.push(Box::new(FeatureDrift::new()));
    }
    if level_on(config, LintId::ModuleTree) {
        out.push(Box::new(ModuleTree::new()));
    }
    if level_on(config, LintId::StaleGitIndex) {
        out.push(Box::new(StaleGitIndex::new()));
    }
    if level_on(config, LintId::UnusedDeps) {
        out.push(Box::new(UnusedDeps::new(
            config.unused_deps.clone().unwrap_or_default(),
            config.unused_deps_overrides(),
        )));
    }
    if level_on(config, LintId::UnusedPub) {
        out.push(Box::new(UnusedPub::new(
            config.unused_pub.clone().unwrap_or_default(),
            config.unused_pub_overrides(),
        )));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::scenarios;

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

[[cli-crate-version.rules]]
command = [\"wasm-bindgen\", \"--version\"]
pattern = \"wasm-bindgen (\\\\S+)\"
crate = \"wasm-bindgen\"

[[architecture.rules]]
from = [\"crates/a/**\"]
deny = [\"crates/b/**\"]

[duplicate-code]
";

    /// Lints that are emitted by the config-audit / suppression pipeline rather
    /// than a [`registry`] entry — they have no `Lint` instance. Every *other*
    /// variant must be produced by the registry. The stale-expect "ran" set
    /// mirrors this split: `apply_suppression` always counts its own two
    /// meta-lints as ran, and the default run adds `config`.
    const META_LINTS: &[LintId] = &[LintId::Config, LintId::StaleExpect, LintId::UnknownLint];

    #[test]
    fn registry_covers_every_non_meta_lint() {
        let config: Config =
            toml::from_str(ALL_LINTS_ENABLED_CONFIG).expect("maximal config parses");
        let produced: std::collections::HashSet<LintId> =
            registry(&config).iter().map(|l| l.id()).collect();

        let mut missing: Vec<&str> = LintId::ALL
            .iter()
            .filter(|id| !META_LINTS.contains(id) && !produced.contains(id))
            .map(|id| id.short())
            .collect();
        missing.sort();
        assert!(
            missing.is_empty(),
            "every non-meta lint in LintId::ALL must be built by registry(); missing: {missing:?}\n\
             Add a line in `registry::registry` wiring the new lint to its config block \
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

    // --- missing-test-files guards (these assets live in this binary crate) ---

    #[test]
    fn every_lint_has_a_message_scenario() {
        let scenarios = scenarios();
        let covered: std::collections::HashSet<&str> =
            scenarios.iter().map(|(_, d)| d.lint.as_ref()).collect();
        let mut missing: Vec<&str> = LintId::ALL
            .iter()
            .map(|id| id.id())
            .filter(|lint| !covered.contains(lint))
            .collect();
        missing.sort();
        assert!(
            missing.is_empty(),
            "every lint in LintId::ALL must appear in messages::scenarios(); missing: {missing:?}\n\
             Add a scenario at the bottom of `scenarios()` in src/messages.rs and \
             corresponding snapshot tests."
        );
    }

    /// Lints with a paired `tests/fixtures/fix__<short>/` directory exercised by
    /// `tests/fix_fixtures.rs`. Only lints that emit a real structural rewrite
    /// (byte-range replacement with `Applicability::MachineApplicable`) belong
    /// here — `--fix` never inserts silence directives, so any lint without a
    /// structural fix has nothing for the fixture driver to assert on.
    ///
    /// Omitted today:
    /// - `file-size`, `crate-size`: no structural fix — heuristic refactoring
    ///   isn't tractable. `--fix` is a no-op; users must split files manually.
    /// - `cli-crate-version`: needs a fake CLI binary the fixture can invoke.
    /// - `stale-git-index`: needs `git ls-files` to disagree with on-disk
    ///   state, which requires an in-tempdir git init/add/rm dance.
    /// - `config`, `unknown-lint`: config-validation diagnostics with no
    ///   structural fix — the remedy is a hand edit of the config / directive.
    /// - `duplicate-code`: advisory by design, never fixturable — resolving a
    ///   duplicate means *extracting* (naming, parameterizing, choosing a
    ///   home), an author decision no machine-applicable rewrite can make.
    /// - `architecture`, `feature-drift`, `module-tree`: the structural fixes
    ///   for these are planned but not yet implemented; once `--fix` rewrites
    ///   them, add fixtures and move them up.
    const FIXTURABLE_LINTS: &[LintId] = &[
        LintId::CentralizedDeps,
        LintId::StaleExpect,
        LintId::UnusedDeps,
        LintId::UnusedPub,
    ];

    #[test]
    fn every_fixturable_lint_has_a_fix_fixture() {
        let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let fixtures = manifest.join("tests/fixtures");
        let mut missing: Vec<String> = Vec::new();
        for id in FIXTURABLE_LINTS {
            let name = id.short().replace('-', "_");
            let dir = fixtures.join(format!("fix__{name}"));
            if !dir.join("input").is_dir() || !dir.join("expected").is_dir() {
                missing.push(format!(
                    "fix__{name} (missing input/ or expected/ for lint `{}`)",
                    id.id()
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

    /// Lints with no `tests/cases/<short>/` directory, each with its reason.
    /// `cli-crate-version` needs an executable fake tool, so it's exercised by
    /// the dedicated `tests/cli_crate_version.rs` harness instead.
    const NO_CASES_DIR: &[LintId] = &[LintId::CliCrateVersion];

    #[test]
    fn every_lint_has_case_fixtures() {
        let cases = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/cases");
        let mut missing: Vec<&str> = LintId::ALL
            .iter()
            .filter(|id| !NO_CASES_DIR.contains(id))
            .filter(|id| !cases.join(id.short()).is_dir())
            .map(|id| id.short())
            .collect();
        missing.sort();
        assert!(
            missing.is_empty(),
            "every lint needs a kebab-case `tests/cases/<short>/` directory (or an entry \
             in NO_CASES_DIR with a reason); missing: {missing:?}\n\
             Note: the directory name must match `LintId::short()` exactly."
        );
    }
}
