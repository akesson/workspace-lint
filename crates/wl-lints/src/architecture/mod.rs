//! Architecture rules — workspace layering enforcement.
//!
//! Each `[[architecture.rules]]` entry has a `from` set of crate-name globs
//! (the importing crate) and a `deny` set of canonical-path globs (forbidden
//! targets), with an optional `exceptions` list of specific canonical paths
//! that bypass the rule.
//!
//! For every `use` binding, every glob import (`use mod::*`), and every
//! fully-qualified code reference (`other_crate::forbidden::Type::call()`) in
//! every workspace module, the check resolves the referenced canonical path,
//! then for each rule whose `from` matches the importing crate, fires a
//! diagnostic when the resolved canonical is in `deny` and not in
//! `exceptions`. A glob import is tested as a representative child of its
//! target module, so a `deny = ["mod::**"]` pattern catches `use mod::*` just
//! as it catches `use mod::Item`. A fully-qualified reference is tested
//! against its canonical and every prefix (so `mod::Type::method()` matches a
//! `mod::Type` deny), and is reported once per `(target, rule)` per module —
//! a violation already reported via its `use` binding is not repeated.
//! Architecture rules govern *production* layering: only each member's
//! primary units (lib / proc-macro / bins) are judged — tests, examples, and
//! benches legitimately reach across layers.
//!
//! Pattern grammar: `::` separates path segments; converted to `/` for
//! globset matching. `*` matches one segment, `**` matches zero or more.
//!
//! The judged substrate is the rustc-extracted reference graph (the engine's
//! [`wl_engine::SemanticModel`]). Canonical paths come from joining each
//! reference to its target's *definition* — so every re-export chain
//! (`pub use` **and** `pub(crate) use`) resolves exactly, and references
//! inside macro expansions are judged (anchored at the invocation site) —
//! the two scope limits the retired syn resolver documented.

use std::path::PathBuf;

use globset::{Glob, GlobMatcher};

use wl_diagnostic::Diagnostic;
use wl_diagnostic::builder::{at_crate, at_line};
use wl_lint_api::config::LintLevel;
use wl_lint_api::{LintContext, LintId, LintImpl, Requirements};

pub mod config;
mod ir;
#[cfg(test)]
mod tests;

pub use config::{ArchitectureConfig, ArchitectureRule};

pub struct Architecture {
    config: ArchitectureConfig,
}

impl Architecture {
    pub fn new(config: ArchitectureConfig) -> Self {
        Self { config }
    }
}

impl LintImpl for Architecture {
    const ID: LintId = LintId::Architecture;
    // Semantic judgment; fast for member names (rule `from` globs match
    // *package* names) and workspace-relative anchor paths.
    const REQUIRES: Requirements = Requirements {
        needs_fast: true,
        needs_semantic: true,
    };

    fn run(&self, cx: &LintContext<'_>) -> Vec<Diagnostic> {
        let Some((fast, semantic)) = cx.semantic_models(Self::ID) else {
            return Vec::new();
        };
        ir::check(&self.config, fast, semantic)
    }
}

/// How a denied path was referenced — governs the diagnostic's verb and display.
#[derive(Clone, Copy)]
enum RefKind<'a> {
    /// An explicit `use` binding; carries the local alias (drives the rename note).
    Use(Option<&'a str>),
    /// A glob import (`use mod::*`); rendered with a trailing `::*`.
    Glob,
    /// A fully-qualified reference in regular code (`mod::Type::method()`).
    Code,
}

/// Where a violation diagnostic anchors: the offending line when the
/// reference has a recorded source, else the crate as a whole.
enum Anchor {
    Line(PathBuf, u32),
    Crate(PathBuf),
}

/// Build a violation diagnostic from backend-independent data — one renderer,
/// so the two backends' messages are byte-identical by construction. `kind`
/// selects the wording and display: `Use`/`Glob` render as "import of" (a
/// glob adds a trailing `::*`), `Code` renders as "reference to" a
/// fully-qualified call site; `Use` also carries the local alias that drives
/// the rename note.
fn build_diagnostic(
    rule: &CompiledRule,
    from_name: &str,
    kind: RefKind<'_>,
    resolved: &[String],
    anchor: Anchor,
    module_display: &str,
) -> Diagnostic {
    let rule_name = rule.name.as_deref().unwrap_or("unnamed");
    // Only an explicit `use` binding has an alias worth a rename note.
    let local_name = match kind {
        RefKind::Use(alias) => alias,
        RefKind::Glob | RefKind::Code => None,
    };
    let imported = match kind {
        // Glob import: display the target module with the wildcard.
        RefKind::Glob => format!("{}::*", resolved.join("::")),
        RefKind::Use(_) | RefKind::Code => resolved.join("::"),
    };
    let verb = match kind {
        RefKind::Use(_) | RefKind::Glob => "import of",
        RefKind::Code => "reference to",
    };
    let msg =
        format!("{verb} `{imported}` from `{from_name}` violates architecture rule `{rule_name}`");

    let spanless = matches!(anchor, Anchor::Crate(_));
    let mut builder = match anchor {
        Anchor::Line(file, line) => at_line(LintId::Architecture.id(), msg, file, line),
        Anchor::Crate(dir) => at_crate(LintId::Architecture.id(), msg, dir),
    };
    // An explicit per-rule severity wins over a blanket `[lints] architecture`
    // override (marked via `level_explicit`); `None` leaves the default `warn`,
    // which the `[lints]` table may then escalate. `allow` rules never compile
    // (see `CompiledRule::compile`), so only `warn`/`deny` reach here.
    if let Some(level) = rule.severity.and_then(|s| s.to_diagnostic_level()) {
        builder = builder.level_explicit(level);
    }

    if let Some(suggest) = &rule.suggest {
        builder = builder.help(suggest.clone());
    } else {
        builder = builder.help(format!(
            "`{imported}` matches deny pattern of rule `{rule_name}`",
        ));
    }

    if let Some(reason) = &rule.reason {
        builder = builder.note(reason.clone());
    }

    // The "imported in module" / "imported locally as ..." note loses
    // most of its value once the diagnostic carries a file:line anchor —
    // the source line is one click away. Keep it only for the rename
    // case (where the local alias is non-obvious) and only as a
    // fallback when the reference has no recorded source. Glob imports
    // (`local_name == None`) have no alias, so the rename note never applies.
    let local_differs =
        local_name.is_some_and(|ln| ln != resolved.last().map(String::as_str).unwrap_or_default());
    if spanless {
        if local_differs {
            builder = builder.note(format!(
                "imported locally as `{}` in module `{module_display}`",
                local_name.unwrap_or_default(),
            ));
        } else {
            builder = builder.note(format!("imported in module `{module_display}`"));
        }
    } else if local_differs {
        builder = builder.note(format!(
            "imported locally as `{}`",
            local_name.unwrap_or_default()
        ));
    }

    builder.build()
}

struct CompiledRule {
    name: Option<String>,
    from: Vec<GlobMatcher>,
    deny: Vec<GlobMatcher>,
    exceptions: Vec<GlobMatcher>,
    severity: Option<LintLevel>,
    reason: Option<String>,
    suggest: Option<String>,
}

impl CompiledRule {
    fn compile(rule: &ArchitectureRule) -> Option<Self> {
        if rule.from.is_empty() || rule.deny.is_empty() {
            return None;
        }
        // `severity = "allow"` mutes the rule entirely — don't compile it.
        if rule.severity == Some(LintLevel::Allow) {
            return None;
        }
        let from = compile_globs(&rule.from, |s| s.to_string());
        let deny = compile_globs(&rule.deny, path_pattern_to_glob_form);
        let exceptions = compile_globs(&rule.exceptions, path_pattern_to_glob_form);
        Some(Self {
            name: rule.name.clone(),
            from,
            deny,
            exceptions,
            severity: rule.severity,
            reason: rule.reason.clone(),
            suggest: rule.suggest.clone(),
        })
    }

    fn matches_from(&self, crate_name: &str) -> bool {
        self.from.iter().any(|g| g.is_match(crate_name))
    }

    fn denies(&self, canonical: &[String]) -> bool {
        let glob_path = segments_to_glob_form(canonical);
        self.deny.iter().any(|g| g.is_match(&glob_path))
    }

    fn is_exception(&self, canonical: &[String]) -> bool {
        let glob_path = segments_to_glob_form(canonical);
        self.exceptions.iter().any(|g| g.is_match(&glob_path))
    }
}

fn compile_globs<F: Fn(&str) -> String>(items: &[String], normalize: F) -> Vec<GlobMatcher> {
    items
        .iter()
        .filter_map(|s| Glob::new(&normalize(s)).ok())
        .map(|g| g.compile_matcher())
        .collect()
}

/// Convert a config-level pattern (`crate::module::*`) to globset's path form
/// (`crate/module/*`) for matching against canonical paths.
///
/// Normalizes cargo-style crate names with hyphens (`data-models`) to their
/// in-code form (`data_models`) so the user's pattern matches the canonical
/// path the resolver stores.
fn path_pattern_to_glob_form(pattern: &str) -> String {
    pattern.replace('-', "_").replace("::", "/")
}

fn segments_to_glob_form(segments: &[String]) -> String {
    segments.join("/")
}

/// The canonical plus every prefix of length ≥ 2, shortest first. A reference to
/// `a::b::Type::method` is also a use of `a::b::Type` and `a::b`, so a rule
/// targeting any of those should fire. Length-1 (bare crate) prefixes are
/// skipped — crate-level layering isn't this lint's grain.
fn canonical_prefixes(canonical: &[String]) -> Vec<Vec<String>> {
    (2..=canonical.len())
        .map(|n| canonical[..n].to_vec())
        .collect()
}
