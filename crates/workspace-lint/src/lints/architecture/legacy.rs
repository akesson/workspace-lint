//! The pre-pivot syn-resolver architecture check, kept intact behind
//! `WL_SEMANTIC_BACKEND=syn` for backend diffing until the migration's
//! deletion PR. Judgment and rendering go through the shared machinery in the
//! parent module ([`CompiledRule`], [`build_diagnostic`]) so the two backends
//! stay byte-identical where their models agree.
//!
//! Known scope limits of this backend:
//!
//! - **`pub(crate) use` re-export hops are invisible** — Tier 2.5 follows
//!   only `pub use` edges.
//! - **References inside macro bodies are not inspected** — only regular-code
//!   ([`Origin::Code`]) references, matching the resolver's macro non-goals.

use std::collections::{HashMap, HashSet};

use syn_workspace::{Origin, ResolvedPath, SourceSpan, Workspace};

use crate::diagnostic::Diagnostic;

use super::{
    Anchor, ArchitectureConfig, CompiledRule, RefKind, build_diagnostic, canonical_prefixes,
    segments_to_glob_form,
};

pub(super) fn check(config: &ArchitectureConfig, workspace: &Workspace) -> Vec<Diagnostic> {
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
            // Within a module, map a denied path's glob form → the set of rule
            // indices that have already reported it: a violation surfaced via a
            // `use` binding (or glob) is recorded here so the fully-qualified-
            // reference pass below doesn't repeat it, and so N call sites of the
            // same denied path collapse to one diagnostic. A map (not a set of
            // `(idx, String)`) lets the code-reference pass probe by borrowed
            // key, with no per-rule string allocation.
            let mut reported: HashMap<String, HashSet<usize>> = HashMap::new();

            // Explicit `use` bindings — the canonical path is the imported item,
            // which is both what we match against `deny` and what we display.
            for binding in &module.use_bindings {
                let canonical = workspace.resolve_canonical(&binding.canonical);
                // Architecture rules govern *cross-crate* layering; a crate
                // referencing its own modules is never a violation.
                if is_own_crate_ref(&canonical, krate) {
                    continue;
                }
                let canonical = canonical.segments().to_vec();
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
                    reported
                        .entry(segments_to_glob_form(&canonical))
                        .or_default()
                        .insert(rule_idx);
                    diagnostics.push(build_diagnostic(
                        rule,
                        from_name,
                        RefKind::Use(Some(binding.local_name.as_str())),
                        &canonical,
                        anchor(workspace, krate, binding.source.as_ref()),
                        &module.canonical.display().to_string(),
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
                if is_own_crate_ref(&canonical, krate) {
                    continue;
                }
                let canonical = canonical.segments().to_vec();
                let mut child = canonical.clone();
                child.push("*".to_string());
                for (rule_idx, rule) in compiled.iter().enumerate() {
                    if !rule.matches_from(from_name) || !rule.denies(&child) {
                        continue;
                    }
                    if rule.is_exception(&child) {
                        continue;
                    }
                    reported
                        .entry(segments_to_glob_form(&child))
                        .or_default()
                        .insert(rule_idx);
                    diagnostics.push(build_diagnostic(
                        rule,
                        from_name,
                        RefKind::Glob,
                        &canonical,
                        anchor(workspace, krate, occ.span.as_ref()),
                        &module.canonical.display().to_string(),
                    ));
                }
            }
            // Fully-qualified code references (`other::forbidden::Type::new()`
            // with no `use`). Only regular-code occurrences — macro-body refs are
            // out of scope (resolver macro non-goals); bare-ident origins carry
            // no resolved path. Each reference is tested against its canonical
            // and every prefix, so `Type::method` matches a `Type` deny, and the
            // shortest denied prefix is what we report — unless an exception lies
            // at or below that denied prefix (so an exception on `a::b::Foo`
            // still covers `a::b::Foo::method` under `a::b::**`, but a broader
            // ancestor like `a::b` does *not* exempt, matching the `use` pass).
            for occ in module
                .occurrences
                .iter()
                .filter(|o| o.origin == Origin::Code)
            {
                let Some(path) = occ.path.as_ref() else {
                    continue;
                };
                let canonical = workspace.resolve_canonical(path);
                // Cross-crate only: a crate's reference to its own modules is
                // never a layering violation (see `use`-binding pass above).
                if is_own_crate_ref(&canonical, krate) {
                    continue;
                }
                let prefixes = canonical_prefixes(canonical.segments());
                // Each prefix's glob form, computed once and reused across rules
                // (the dedup probe below would otherwise re-allocate per rule).
                let prefix_keys: Vec<String> =
                    prefixes.iter().map(|p| segments_to_glob_form(p)).collect();
                for (rule_idx, rule) in compiled.iter().enumerate() {
                    if !rule.matches_from(from_name) {
                        continue;
                    }
                    let Some(denied_idx) = prefixes.iter().position(|p| rule.denies(p)) else {
                        continue;
                    };
                    // Only an exception *at or below* the denied prefix exempts
                    // the reference — a shorter ancestor must not (that would
                    // diverge from the `use` pass, which checks the item itself).
                    if prefixes[denied_idx..].iter().any(|p| rule.is_exception(p)) {
                        continue;
                    }
                    // Skip if a `use`/glob or an earlier reference already
                    // reported *any* prefix of this reference for the rule. Done
                    // per-prefix (not just the matched one) so a `use`-binding
                    // recorded at item granularity still dedups a reference a
                    // broad `**` rule would otherwise report at module
                    // granularity — and so N call sites collapse to one.
                    if prefix_keys.iter().any(|k| {
                        reported
                            .get(k)
                            .is_some_and(|rules| rules.contains(&rule_idx))
                    }) {
                        continue;
                    }
                    reported
                        .entry(prefix_keys[denied_idx].clone())
                        .or_default()
                        .insert(rule_idx);
                    diagnostics.push(build_diagnostic(
                        rule,
                        from_name,
                        RefKind::Code,
                        &prefixes[denied_idx],
                        anchor(workspace, krate, occ.span.as_ref()),
                        &module.canonical.display().to_string(),
                    ));
                }
            }
        }
    }

    diagnostics
}

/// A reference is "own-crate" when its resolved canonical is rooted in the crate
/// doing the referencing. Architecture rules govern *cross-crate* layering, so a
/// crate touching its own modules is never a violation — even under a wildcard
/// rule like `from = ["*"]`, `deny = ["*::internal::**"]`. Both sides are already
/// in code form (underscored), so the comparison is exact.
fn is_own_crate_ref(canonical: &ResolvedPath, krate: &syn_workspace::Crate) -> bool {
    canonical.crate_name() == Some(krate.code_name().as_str())
}

/// Prefer line-accurate anchoring at the offending `use` line (the span landed
/// in syn-workspace 0.4.0). For references built outside the parser (test
/// mocks, future synthesized sources) fall back to a workspace-relative crate
/// anchor.
fn anchor(
    workspace: &Workspace,
    krate: &syn_workspace::Crate,
    span: Option<&SourceSpan>,
) -> Anchor {
    match span {
        Some(span) => Anchor::Line(workspace.crate_relative_path(&span.file), span.line),
        None => Anchor::Crate(workspace.crate_relative_path(&krate.manifest_dir)),
    }
}
