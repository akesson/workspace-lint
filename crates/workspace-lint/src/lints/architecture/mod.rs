//! Architecture rules — workspace layering enforcement.
//!
//! Each `[[architecture.rules]]` entry has a `from` set of crate-name globs
//! (the importing crate) and a `deny` set of canonical-path globs (forbidden
//! targets), with an optional `exceptions` list of specific canonical paths
//! that bypass the rule.
//!
//! For every `use` binding, every glob import (`use mod::*`), and every
//! fully-qualified code reference (`other_crate::forbidden::Type::call()`) in
//! every workspace module, the check resolves the referenced canonical path
//! through Tier 2.5's re-export index, then for each rule whose `from` matches
//! the importing crate, fires a diagnostic when the resolved canonical is in
//! `deny` and not in `exceptions`. A glob import is tested as a representative
//! child of its target module, so a `deny = ["mod::**"]` pattern catches
//! `use mod::*` just as it catches `use mod::Item`. A fully-qualified reference
//! is tested against its canonical and every prefix (so `mod::Type::method()`
//! matches a `mod::Type` deny), and is reported once per `(target, rule)` per
//! module — a violation already reported via its `use` binding is not repeated.
//!
//! Pattern grammar: `::` separates path segments; converted to `/` for
//! globset matching. `*` matches one segment, `**` matches zero or more.
//!
//! ## Known scope limits
//!
//! - **`pub(crate) use` re-export hops are invisible** — Tier 2.5 follows
//!   only `pub use` edges.
//! - **References inside macro bodies are not inspected** — only regular-code
//!   ([`Origin::Code`]) references, matching the resolver's macro non-goals.

use std::collections::HashSet;

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
            // One report per `(matched-canonical, rule)` within a module: a
            // violation surfaced via a `use` binding (or glob) is recorded here
            // so the fully-qualified-reference pass below doesn't repeat it, and
            // so N call sites of the same denied path collapse to one diagnostic.
            let mut reported: HashSet<(usize, String)> = HashSet::new();

            // Explicit `use` bindings — the canonical path is the imported item,
            // which is both what we match against `deny` and what we display.
            for binding in &module.use_bindings {
                let canonical = workspace.resolve_canonical(&binding.canonical);
                for (rule_idx, rule) in compiled.iter().enumerate() {
                    if !rule.matches_from(from_name) || !rule.denies(&canonical) {
                        continue;
                    }
                    if rule.is_exception(&canonical) {
                        continue;
                    }
                    // Record even when a duplicate `use` wouldn't re-fire (it
                    // can't, source-unique) — the point is to claim this
                    // (canonical, rule) so the code-reference pass skips it.
                    reported.insert((rule_idx, path_to_glob_form(&canonical)));
                    diagnostics.push(build_diagnostic(
                        rule,
                        workspace,
                        krate,
                        module,
                        binding.source.as_ref(),
                        RefKind::Use(Some(binding.local_name.as_str())),
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
                for (rule_idx, rule) in compiled.iter().enumerate() {
                    if !rule.matches_from(from_name) || !rule.denies(&child) {
                        continue;
                    }
                    if rule.is_exception(&child) {
                        continue;
                    }
                    reported.insert((rule_idx, path_to_glob_form(&child)));
                    diagnostics.push(build_diagnostic(
                        rule,
                        workspace,
                        krate,
                        module,
                        occ.span.as_ref(),
                        RefKind::Glob,
                        &canonical,
                    ));
                }
            }
            // Fully-qualified code references (`other::forbidden::Type::new()`
            // with no `use`). Only regular-code occurrences — macro-body refs are
            // out of scope (resolver macro non-goals); bare-ident origins carry
            // no resolved path. Each reference is tested against its canonical
            // and every prefix, so `Type::method` matches a `Type` deny, and the
            // shortest denied prefix is what we report — unless any prefix is an
            // exception (then the whole reference is exempt, so an exception on
            // `a::b::Foo` still covers `a::b::Foo::method` even under `a::b::**`).
            for occ in module
                .occurrences
                .iter()
                .filter(|o| o.origin == Origin::Code)
            {
                let Some(path) = occ.path.as_ref() else {
                    continue;
                };
                let canonical = workspace.resolve_canonical(path);
                let prefixes = canonical_prefixes(&canonical);
                for (rule_idx, rule) in compiled.iter().enumerate() {
                    if !rule.matches_from(from_name) {
                        continue;
                    }
                    let Some(denied) = prefixes.iter().find(|p| rule.denies(p)) else {
                        continue;
                    };
                    if prefixes.iter().any(|p| rule.is_exception(p)) {
                        continue;
                    }
                    // Skip if a `use`/glob or an earlier reference already
                    // reported *any* prefix of this reference for the rule. Done
                    // per-prefix (not just the matched one) so a `use`-binding
                    // recorded at item granularity still dedups a reference a
                    // broad `**` rule would otherwise report at module
                    // granularity — and so N call sites collapse to one.
                    if prefixes
                        .iter()
                        .any(|p| reported.contains(&(rule_idx, path_to_glob_form(p))))
                    {
                        continue;
                    }
                    reported.insert((rule_idx, path_to_glob_form(denied)));
                    diagnostics.push(build_diagnostic(
                        rule,
                        workspace,
                        krate,
                        module,
                        occ.span.as_ref(),
                        RefKind::Code,
                        denied,
                    ));
                }
            }
        }
    }

    diagnostics
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

/// The canonical plus every prefix of length ≥ 2, shortest first. A reference to
/// `a::b::Type::method` is also a use of `a::b::Type` and `a::b`, so a rule
/// targeting any of those should fire. Length-1 (bare crate) prefixes are
/// skipped — crate-level layering isn't this lint's grain.
fn canonical_prefixes(canonical: &ResolvedPath) -> Vec<ResolvedPath> {
    let segs = canonical.segments();
    (2..=segs.len())
        .map(|n| ResolvedPath::new(segs[..n].iter().cloned()))
        .collect()
}

/// Build a violation diagnostic. `kind` selects the wording and display:
/// `Use`/`Glob` render as "import of" (a glob adds a trailing `::*`), `Code`
/// renders as "reference to" a fully-qualified call site; `Use` also carries the
/// local alias that drives the rename note. `span` anchors the diagnostic at the
/// offending line when available.
fn build_diagnostic(
    rule: &CompiledRule,
    workspace: &Workspace,
    krate: &syn_workspace::Crate,
    module: &Module,
    span: Option<&SourceSpan>,
    kind: RefKind,
    resolved: &ResolvedPath,
) -> Diagnostic {
    let rule_name = rule.name.as_deref().unwrap_or("unnamed");
    // Only an explicit `use` binding has an alias worth a rename note.
    let local_name = match kind {
        RefKind::Use(alias) => alias,
        RefKind::Glob | RefKind::Code => None,
    };
    let imported = match kind {
        // Glob import: display the target module with the wildcard.
        RefKind::Glob => format!("{}::*", resolved.display()),
        RefKind::Use(_) | RefKind::Code => resolved.display().to_string(),
    };
    let verb = match kind {
        RefKind::Use(_) | RefKind::Glob => "import of",
        RefKind::Code => "reference to",
    };
    let msg = format!(
        "{verb} `{imported}` from `{}` violates architecture rule `{rule_name}`",
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
