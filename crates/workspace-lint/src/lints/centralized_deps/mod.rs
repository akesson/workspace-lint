use std::collections::BTreeSet;
use syn_workspace::Workspace;
use syn_workspace::manifest::{DepSection, Manifest};
use syn_workspace::toml_edit::Item;

use crate::diagnostic::builder::at_crate;
use crate::diagnostic::{Applicability, Diagnostic, Span, Suggestion};
use crate::lints::{Lint, LintContext, LintId, Requirements};

#[cfg(test)]
mod tests;

pub(crate) struct CentralizedDeps;

impl CentralizedDeps {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CentralizedDeps {
    fn default() -> Self {
        Self::new()
    }
}

impl Lint for CentralizedDeps {
    fn id(&self) -> LintId {
        LintId::CentralizedDeps
    }

    fn requirements(&self) -> Requirements {
        Requirements {
            needs_workspace: true,
        }
    }

    fn check(&self, cx: &LintContext<'_>) -> Vec<Diagnostic> {
        let workspace = cx
            .workspace
            .expect("centralized-deps lint requires Workspace (Requirements::needs_workspace)");
        check(workspace)
    }
}

pub(crate) fn check(workspace: &Workspace) -> Vec<Diagnostic> {
    let lint_id = LintId::CentralizedDeps.id();
    let workspace_dep_names = workspace.root_manifest().workspace_dep_names();

    let mut diagnostics = Vec::new();

    for krate in workspace.members() {
        let manifest = krate.manifest();
        let mut crate_errors: Vec<String> = Vec::new();
        let mut suggestions: Vec<Suggestion> = Vec::new();

        for section in DepSection::member_sections() {
            for (name, item) in manifest.deps(section) {
                if let Some(msg) = check_dep(name, item, section, &workspace_dep_names) {
                    crate_errors.push(msg);
                    if let Some(s) = build_rewrite_suggestion(manifest, section, name) {
                        suggestions.push(s);
                    }
                }
            }
        }

        if !crate_errors.is_empty() {
            let n = crate_errors.len();
            // Workspace-relative paths everywhere — both the in-message
            // path and the suppression anchor — so a per-Cargo.toml
            // directive (`# workspace-lint: allow(centralized-deps)`)
            // can actually match the diagnostic's `SilenceAnchor::Crate`.
            // Force forward-slash separators in the rendered string so
            // Windows runs of this lint produce the same diagnostic text
            // as Linux/macOS (snapshot fixtures lock that in).
            let manifest_path_rel = workspace.crate_relative_path(manifest.path());
            let manifest_dir_rel = workspace.crate_relative_path(&krate.manifest_dir);
            let cargo_path_str = manifest_path_rel.display().to_string().replace('\\', "/");
            let mut builder = at_crate(
                lint_id,
                format!(
                    "{n} dependenc{} in {cargo_path_str} should use `workspace = true`",
                    if n == 1 { "y" } else { "ies" },
                ),
                manifest_dir_rel,
            );
            for err in crate_errors {
                builder = builder.help(err);
            }
            for s in suggestions {
                builder = builder.suggestion(s);
            }
            diagnostics.push(builder.build());
        }
    }

    diagnostics
}

/// Build a `MachineApplicable` byte-range replacement that turns
/// `<name> = "..."` or `<name> = { ... }` into
/// `<name> = { workspace = true[, preserved keys] }`. Returns `None` for
/// entries this lint can't safely rewrite.
fn build_rewrite_suggestion(
    manifest: &Manifest,
    section: DepSection,
    dep_name: &str,
) -> Option<Suggestion> {
    let location = manifest.locate_dep(section, dep_name)?;
    let replacement = manifest.format_workspace_dep(section, dep_name)?;
    let original = &manifest.raw()[location.byte_start as usize..location.byte_end as usize];
    if replacement == original {
        return None;
    }
    Some(Suggestion {
        span: Span {
            file: manifest.path().to_path_buf(),
            line_start: location.line,
            line_end: location.line,
            col_start: 1,
            col_end: 1,
            byte_start: location.byte_start,
            byte_end: location.byte_end,
        },
        message: format!("use {{ workspace = true }} for `{dep_name}`"),
        replacement,
        applicability: Applicability::MachineApplicable,
    })
}

fn check_dep(
    name: &str,
    item: &Item,
    section: DepSection,
    workspace_deps: &BTreeSet<String>,
) -> Option<String> {
    let section_str = section.as_str();

    if let Some(version) = item.as_str() {
        if workspace_deps.contains(name) {
            return Some(format!(
                "[{section_str}] {name}: has own version \"{version}\" — use {{ workspace = true }} instead"
            ));
        }
        return Some(format!(
            "[{section_str}] {name}: version \"{version}\" not in [workspace.dependencies] — add it there and use {{ workspace = true }}"
        ));
    }

    let table = item.as_table_like()?;

    if table
        .get("workspace")
        .and_then(Item::as_bool)
        .unwrap_or(false)
    {
        return None;
    }

    if table.contains_key("path") {
        return None;
    }

    if let Some(version) = table.get("version").and_then(Item::as_str) {
        if workspace_deps.contains(name) {
            return Some(format!(
                "[{section_str}] {name}: has own version \"{version}\" — use {{ workspace = true }} instead"
            ));
        }
        return Some(format!(
            "[{section_str}] {name}: version \"{version}\" not in [workspace.dependencies] — add it there and use {{ workspace = true }}"
        ));
    }

    if table.contains_key("git") {
        if workspace_deps.contains(name) {
            return Some(format!(
                "[{section_str}] {name}: has own git source — use {{ workspace = true }} instead"
            ));
        }
        return None;
    }

    None
}
