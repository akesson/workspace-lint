//! Architecture rules — workspace layering enforcement.
//!
//! Each `[[architecture.rules]]` entry has a `from` set of crate-name globs
//! (the importing crate) and a `deny` set of canonical-path globs (forbidden
//! targets), with an optional `exceptions` list of specific canonical paths
//! that bypass the rule.
//!
//! For every `use` binding in every workspace module, the check resolves the
//! binding's canonical path through Tier 2.5's re-export index, then for
//! each rule whose `from` matches the importing crate, fires a diagnostic
//! when the resolved canonical is in `deny` and not in `exceptions`.
//!
//! Pattern grammar: `::` separates path segments; converted to `/` for
//! globset matching. `*` matches one segment, `**` matches zero or more.
//!
//! ## Known scope limits
//!
//! - **Only `use` bindings are inspected.** Fully-qualified call sites like
//!   `other_crate::forbidden::Type::call()` without a `use` will *not* fire.
//! - **`pub(crate) use` re-export hops are invisible** — Tier 2.5 follows
//!   only `pub use` edges.

use globset::{Glob, GlobMatcher};
use syn_workspace::{Module, ResolvedPath, Workspace};

use crate::diagnostic::Diagnostic;
use crate::diagnostic::Level;
use crate::diagnostic::builder::at_crate;
use crate::lints::{Lint, LintContext, LintId, Requirements};

pub mod config;
#[cfg(test)]
mod tests;

pub(crate) use config::{ArchSeverity, ArchitectureConfig, ArchitectureRule};

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
        for (module, binding) in target.root.walk_use_bindings() {
            let canonical = workspace.resolve_canonical(&binding.canonical);
            for rule in &compiled {
                if !rule.matches_from(from_name) {
                    continue;
                }
                if !rule.denies(&canonical) {
                    continue;
                }
                if rule.is_exception(&canonical) {
                    continue;
                }
                diagnostics.push(build_diagnostic(rule, krate, module, binding, &canonical));
            }
        }
    }

    diagnostics
}

fn build_diagnostic(
    rule: &CompiledRule,
    krate: &syn_workspace::Crate,
    module: &Module,
    binding: &syn_workspace::resolve::use_tree::UseBinding,
    resolved: &ResolvedPath,
) -> Diagnostic {
    let rule_name = rule.name.as_deref().unwrap_or("unnamed");
    let msg = format!(
        "import of `{}` from `{}` violates architecture rule `{}`",
        resolved.display(),
        krate.name,
        rule_name,
    );

    let mut builder = at_crate(LintId::Architecture.id(), msg, krate.manifest_dir.clone()).level(
        match rule.severity {
            ArchSeverity::Warn => Level::Warn,
            ArchSeverity::Deny => Level::Deny,
        },
    );

    if let Some(suggest) = &rule.suggest {
        builder = builder.help(suggest.clone());
    } else {
        builder = builder.help(format!(
            "`{}` matches deny pattern of rule `{rule_name}`",
            resolved.display(),
        ));
    }

    if let Some(reason) = &rule.reason {
        builder = builder.note(reason.clone());
    }

    if binding.local_name != resolved.segments().last().cloned().unwrap_or_default() {
        builder = builder.note(format!(
            "imported locally as `{}` in module `{}`",
            binding.local_name,
            module.canonical.display(),
        ));
    } else {
        builder = builder.note(format!(
            "imported in module `{}`",
            module.canonical.display(),
        ));
    }

    builder.build()
}

struct CompiledRule {
    name: Option<String>,
    from: Vec<GlobMatcher>,
    deny: Vec<GlobMatcher>,
    exceptions: Vec<GlobMatcher>,
    severity: ArchSeverity,
    reason: Option<String>,
    suggest: Option<String>,
}

impl CompiledRule {
    fn compile(rule: &ArchitectureRule) -> Option<Self> {
        if rule.from.is_empty() || rule.deny.is_empty() {
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
