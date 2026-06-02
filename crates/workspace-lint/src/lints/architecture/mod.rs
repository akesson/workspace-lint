//! Architecture rules — workspace layering enforcement.
//!
//! Each `[[architecture.rules]]` entry has a `from` set of crate-name globs
//! (the importing crate) and a `deny` set of canonical-path globs (forbidden
//! targets), with an optional `exceptions` list of specific canonical paths
//! that bypass the rule.
//!
//! For every `use` binding (and every glob import `use mod::*`) in every
//! workspace module, the check resolves the imported canonical path through
//! Tier 2.5's re-export index, then for each rule whose `from` matches the
//! importing crate, fires a diagnostic when the resolved canonical is in
//! `deny` and not in `exceptions`. A glob import is tested as a representative
//! child of its target module, so a `deny = ["mod::**"]` pattern catches
//! `use mod::*` just as it catches `use mod::Item`.
//!
//! Pattern grammar: `::` separates path segments; converted to `/` for
//! globset matching. `*` matches one segment, `**` matches zero or more.
//!
//! ## Known scope limits
//!
//! - **Only `use` bindings and glob imports are inspected.** Fully-qualified
//!   call sites like `other_crate::forbidden::Type::call()` without a `use`
//!   will *not* fire.
//! - **`pub(crate) use` re-export hops are invisible** — Tier 2.5 follows
//!   only `pub use` edges.

use globset::{Glob, GlobMatcher};
use syn_workspace::{Module, Origin, ResolvedPath, SourceSpan, Workspace};

use crate::config::LintLevel;
use crate::diagnostic::Diagnostic;
use crate::diagnostic::builder::{at_crate, at_line};
use crate::lints::{Lint, LintContext, LintId, Requirements};

pub mod config;
#[cfg(test)]
mod tests;

pub(crate) use config::{ArchitectureConfig, ArchitectureRule};

pub(crate) struct Architecture {
    config: ArchitectureConfig,
}

impl Architecture {
    pub fn new(config: ArchitectureConfig) -> Self {
        Self { config }
    }
}

impl Lint for Architecture {
    fn id(&self) -> LintId {
        LintId::Architecture
    }

    fn requirements(&self) -> Requirements {
        Requirements {
            needs_workspace: true,
        }
    }

    fn check(&self, cx: &LintContext<'_>) -> Vec<Diagnostic> {
        let workspace = cx
            .workspace
            .expect("architecture lint requires Workspace (Requirements::needs_workspace)");
        check(&self.config, workspace)
    }
}

pub(crate) fn check(config: &ArchitectureConfig, workspace: &Workspace) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let compiled: Vec<CompiledRule> = config
        .rules
        .iter()
        .filter_map(CompiledRule::compile)
        .collect();
    if compiled.is_empty() {
        return diagnostics;
    }

    // Architecture rules govern production layering — apply to each member's
    // primary unit (lib / proc-macro / main bin) only. Tests, examples,
    // benches, and build scripts legitimately reach across layers and
    // shouldn't enforce production constraints.
    for (krate, target) in workspace.primary_units() {
        let from_name = krate.name.as_str();
        for module in target.root.walk() {
            // Explicit `use` bindings — the canonical path is the imported item,
            // which is both what we match against `deny` and what we display.
            for binding in &module.use_bindings {
                let canonical = workspace.resolve_canonical(&binding.canonical);
                for rule in &compiled {
                    if !rule.matches_from(from_name) || !rule.denies(&canonical) {
                        continue;
                    }
                    if rule.is_exception(&canonical) {
                        continue;
                    }
                    diagnostics.push(build_diagnostic(
                        rule,
                        workspace,
                        krate,
                        module,
                        binding.source.as_ref(),
                        Some(binding.local_name.as_str()),
                        &canonical,
                    ));
                }
            }
            // Glob imports `use mod::*` — matched as a representative child of
            // the target module so a `deny = ["mod::**"]` pattern (which targets
            // children, not the bare module) catches the wildcard import. The
            // module prefix is what we display.
            for occ in module
                .occurrences
                .iter()
                .filter(|o| o.origin == Origin::GlobUse)
            {
                let Some(prefix) = occ.path.as_ref() else {
                    continue;
                };
                let canonical = workspace.resolve_canonical(prefix);
                let child = ResolvedPath::new(
                    canonical
                        .segments()
                        .iter()
                        .cloned()
                        .chain(std::iter::once("*".to_string())),
                );
                for rule in &compiled {
                    if !rule.matches_from(from_name) || !rule.denies(&child) {
                        continue;
                    }
                    if rule.is_exception(&child) {
                        continue;
                    }
                    diagnostics.push(build_diagnostic(
                        rule,
                        workspace,
                        krate,
                        module,
                        occ.span.as_ref(),
                        None,
                        &canonical,
                    ));
                }
            }
        }
    }

    diagnostics
}

/// Build a violation diagnostic. `local_name` is `Some(alias)` for an explicit
/// `use` binding and `None` for a glob import (`use mod::*`); the latter is
/// rendered with a trailing `::*`. `span` anchors the diagnostic at the
/// offending `use` line when available.
fn build_diagnostic(
    rule: &CompiledRule,
    workspace: &Workspace,
    krate: &syn_workspace::Crate,
    module: &Module,
    span: Option<&SourceSpan>,
    local_name: Option<&str>,
    resolved: &ResolvedPath,
) -> Diagnostic {
    let rule_name = rule.name.as_deref().unwrap_or("unnamed");
    let imported = match local_name {
        // Glob import: display the target module with the wildcard.
        None => format!("{}::*", resolved.display()),
        Some(_) => resolved.display().to_string(),
    };
    let msg = format!(
        "import of `{imported}` from `{}` violates architecture rule `{rule_name}`",
        krate.name,
    );

    // Prefer line-accurate anchoring at the offending `use` line (the
    // span landed in syn-workspace 0.4.0). For references built outside the
    // parser (test mocks, future synthesized sources) fall back to a
    // workspace-relative crate anchor.
    let mut builder = match span {
        Some(span) => at_line(
            LintId::Architecture.id(),
            msg,
            workspace.crate_relative_path(&span.file),
            span.line,
        ),
        None => at_crate(
            LintId::Architecture.id(),
            msg,
            workspace.crate_relative_path(&krate.manifest_dir),
        ),
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
    let local_differs = local_name.is_some_and(|ln| {
        ln != resolved
            .segments()
            .last()
            .map(String::as_str)
            .unwrap_or_default()
    });
    if span.is_none() {
        if local_differs {
            builder = builder.note(format!(
                "imported locally as `{}` in module `{}`",
                local_name.unwrap_or_default(),
                module.canonical.display(),
            ));
        } else {
            builder = builder.note(format!(
                "imported in module `{}`",
                module.canonical.display(),
            ));
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

    fn denies(&self, canonical: &ResolvedPath) -> bool {
        let glob_path = path_to_glob_form(canonical);
        self.deny.iter().any(|g| g.is_match(&glob_path))
    }

    fn is_exception(&self, canonical: &ResolvedPath) -> bool {
        let glob_path = path_to_glob_form(canonical);
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

fn path_to_glob_form(path: &ResolvedPath) -> String {
    path.segments().join("/")
}
