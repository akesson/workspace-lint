//! Strongly-typed config primitives shared across lint config structs:
//! lint levels, the `[lints]` table, and glob patterns that validate at
//! deserialize time.

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, de};
use std::collections::HashMap;

use crate::LintId;
use wl_diagnostic::Level;

/// A configured lint level. Unlike [`wl_diagnostic::Level`] (which only
/// describes an *emitted* diagnostic) this carries `Allow`, the off state:
/// an `Allow`-ed lint never runs and its diagnostics are dropped before
/// rendering.
#[derive(Deserialize, Default, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LintLevel {
    Allow,
    #[default]
    Warn,
    Deny,
}

impl LintLevel {
    /// The emitted-diagnostic level, or `None` when `Allow` (drop it).
    pub fn to_diagnostic_level(self) -> Option<Level> {
        match self {
            LintLevel::Allow => None,
            LintLevel::Warn => Some(Level::Warn),
            LintLevel::Deny => Some(Level::Deny),
        }
    }
}

/// The parsed `[lints]` table: a global `default` plus per-lint overrides.
/// Keyed by [`LintId`] so a typo (`unused-dep`) can't silently masquerade as
/// a real lint — unknown names are collected by the config audit and surfaced
/// as `unknown-lint` diagnostics rather than dropped.
#[derive(Default, Debug, Clone)]
pub struct LintLevels {
    /// Baseline level for every lint when it has no per-lint override.
    pub default: Option<LintLevel>,
    /// Per-lint overrides (the reserved `default` key is pulled out above).
    pub overrides: HashMap<LintId, LintLevel>,
}

impl<'de> Deserialize<'de> for LintLevels {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Values must be valid `LintLevel`s (a bad string fails fast); the
        // reserved `default` key sets the baseline; unknown lint names are
        // left for the audit (`config::audit`) to diagnose, not dropped here.
        let raw: HashMap<String, LintLevel> = HashMap::deserialize(deserializer)?;
        let mut out = LintLevels::default();
        for (key, level) in raw {
            if key == "default" {
                out.default = Some(level);
            } else if let Some(id) = LintId::from_short(&key) {
                out.overrides.insert(id, level);
            }
        }
        Ok(out)
    }
}

impl LintLevels {
    /// Effective level for a lint: per-lint override → global `default` →
    /// built-in `Warn`. The config-validation lints (`config`,
    /// `unknown-lint`) are never lowered below `Warn` by a *global*
    /// `default = "allow"`; only an explicit per-lint entry can silence them,
    /// so a blanket allow can't quietly hide a broken config.
    pub fn effective(&self, id: LintId) -> LintLevel {
        if let Some(level) = self.overrides.get(&id) {
            return *level;
        }
        Self::floor_meta(id, self.default.unwrap_or(LintLevel::Warn))
    }

    /// Apply the config-validation meta-floor: a *baseline* `allow` (a global
    /// or per-crate `default`, as opposed to an explicit per-lint entry) can't
    /// silence the `config` / `unknown-lint` lints, so a blanket allow can't
    /// quietly hide a broken config. Pass a baseline level through here; an
    /// explicit per-lint override is honored verbatim and never floored.
    pub fn floor_meta(id: LintId, base: LintLevel) -> LintLevel {
        if base == LintLevel::Allow && matches!(id, LintId::Config | LintId::UnknownLint) {
            LintLevel::Warn
        } else {
            base
        }
    }

    /// Whether the user wrote an explicit per-lint entry for `id` (as opposed
    /// to relying on the `default`). Used to decide when a policy lint that's
    /// been leveled-on but given no config table is a real mistake worth a
    /// `config` diagnostic.
    pub fn has_override(&self, id: LintId) -> bool {
        self.overrides.contains_key(&id)
    }
}

/// A glob pattern that is compiled at deserialize time, so an invalid pattern
/// is reported once — uniformly, citing the offending text — instead of each
/// lint re-implementing `Glob::new(...).unwrap_or_else(exit)`.
#[derive(Debug, Clone)]
pub struct GlobPattern(Glob);

impl GlobPattern {
    /// Compile a pattern. Reached via [`GlobPattern::from_cli`] on the CLI
    /// `check` path (raw `--glob` strings) and the test `From<&str>` helper.
    pub fn new(pattern: &str) -> Result<Self, globset::Error> {
        Glob::new(pattern).map(GlobPattern)
    }

    /// Compile a CLI-provided pattern, exiting with a clear message on error.
    /// The `check` subcommands have no config file to anchor a diagnostic at,
    /// so an unusable glob is a fail-fast error, mirroring the config path.
    /// Used by the lints' `from_cli` constructors.
    pub fn from_cli(pattern: &str) -> Self {
        Self::new(pattern).unwrap_or_else(|e| {
            eprintln!("error: invalid glob `{pattern}`: {e}");
            std::process::exit(2);
        })
    }

    /// The original pattern text. Cross-crate comparisons go through the
    /// [`PartialEq`]`<&str>` impl below rather than this accessor.
    pub fn as_str(&self) -> &str {
        self.0.glob()
    }

    /// The compiled `globset::Glob` (for building a `GlobSet` or a matcher).
    pub fn compiled(&self) -> &Glob {
        &self.0
    }
}

/// Build one `GlobSet` from already-compiled patterns, or `None` when there
/// are no patterns — the "no filter configured" state callers branch on.
/// Callers that want an (empty, matches-nothing) set regardless chain
/// `.unwrap_or_default()`. The single home for the `GlobSetBuilder` loop
/// every lint used to hand-roll.
pub fn glob_set<'a>(patterns: impl IntoIterator<Item = &'a GlobPattern>) -> Option<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    let mut any = false;
    for pattern in patterns {
        builder.add(pattern.compiled().clone());
        any = true;
    }
    any.then(|| {
        builder
            .build()
            .unwrap_or_else(|e| crate::util::fail(format!("failed to build glob filter: {e}")))
    })
}

impl<'de> Deserialize<'de> for GlobPattern {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let pattern = String::deserialize(deserializer)?;
        Glob::new(&pattern)
            .map(GlobPattern)
            .map_err(|e| de::Error::custom(format!("invalid glob `{pattern}`: {e}")))
    }
}

impl PartialEq<str> for GlobPattern {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for GlobPattern {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

/// Concise construction in tests; panics on an invalid literal (a test bug).
#[cfg(test)]
impl From<&str> for GlobPattern {
    fn from(pattern: &str) -> Self {
        Self::new(pattern).expect("test glob literal must be valid")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_set_returns_none_for_empty() {
        assert!(glob_set(&[]).is_none());
    }

    #[test]
    fn glob_set_matches_canonical_path_patterns() {
        let set = glob_set(&[GlobPattern::from("*Error")]).unwrap();
        assert!(set.is_match("MyError"));
        assert!(!set.is_match("Thing"));
    }

    #[test]
    fn globs_glob_set_is_empty_set_for_no_patterns() {
        assert!(!Globs::default().glob_set().is_match("anything"));
    }

    #[test]
    fn per_crate_resolves_override_then_global() {
        let pc = PerCrate::new(
            "global",
            [("special".to_string(), "override")].into_iter().collect(),
        );
        assert_eq!(*pc.for_crate("special"), "override");
        assert_eq!(*pc.for_crate("other"), "global");
    }
}

/// One or more glob patterns: accepts either a bare string or a list, so
/// `depends-on = "**/*.rs"` and `depends-on = ["a", "b"]` both parse.
#[derive(Debug, Clone, Default)]
pub struct Globs(pub Vec<GlobPattern>);

#[cfg(test)]
impl From<&str> for Globs {
    fn from(pattern: &str) -> Self {
        Globs(vec![GlobPattern::from(pattern)])
    }
}

impl Globs {
    pub fn iter(&self) -> std::slice::Iter<'_, GlobPattern> {
        self.0.iter()
    }

    /// One matches-nothing-when-empty `GlobSet` over the patterns — the
    /// always-a-set flavor of [`glob_set`] for include/exclude lists where
    /// "no patterns" and "matches nothing" coincide.
    pub fn glob_set(&self) -> GlobSet {
        glob_set(self.iter()).unwrap_or_default()
    }
}

/// A lint's config resolved per crate: workspace-wide params plus per-crate
/// `[crates.<name>.<lint>]` sections, each *wholesale* replacing the global
/// config for that crate. The shared home for the `global` / `per_crate`
/// field pair and the `get(...).unwrap_or(global)` resolution every
/// crate-scoped lint used to carry.
pub struct PerCrate<C> {
    global: C,
    overrides: HashMap<String, C>,
}

impl<C> PerCrate<C> {
    pub fn new(global: C, overrides: HashMap<String, C>) -> Self {
        Self { global, overrides }
    }

    /// The effective config for a crate (keyed by Cargo package name):
    /// its override, or the global config.
    pub fn for_crate(&self, name: &str) -> &C {
        self.overrides.get(name).unwrap_or(&self.global)
    }
}

impl<'de> Deserialize<'de> for Globs {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum OneOrMany {
            One(GlobPattern),
            Many(Vec<GlobPattern>),
        }
        Ok(match OneOrMany::deserialize(deserializer)? {
            OneOrMany::One(g) => Globs(vec![g]),
            OneOrMany::Many(v) => Globs(v),
        })
    }
}
