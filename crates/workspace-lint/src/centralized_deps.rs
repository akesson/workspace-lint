use crate::diagnostic::builder::at_crate;
use crate::diagnostic::{Applicability, Diagnostic, Span, Suggestion};
use std::collections::BTreeSet;
use syn_workspace::Workspace;
use syn_workspace::manifest::{DepSection, Manifest};
use syn_workspace::toml_edit::Item;

pub const LINT: &str = crate::lints::LintId::CentralizedDeps.id();

pub fn check(workspace: &Workspace) -> Vec<Diagnostic> {
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
            let mut builder = at_crate(
                LINT,
                format!(
                    "{n} dependenc{} in {} should use `workspace = true`",
                    if n == 1 { "y" } else { "ies" },
                    manifest.path().display()
                ),
                krate.manifest_dir.clone(),
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
/// entries this lint can't safely rewrite (multi-line inline tables,
/// `[dependencies.<name>]` block form), in which case the diagnostic still
/// fires but without a `--fix`-applicable suggestion.
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

/// Decide whether a dep entry violates the "use workspace = true" rule.
/// Returns the human-readable explanation string, or `None` if the entry
/// is acceptable.
fn check_dep(
    name: &str,
    item: &Item,
    section: DepSection,
    workspace_deps: &BTreeSet<String>,
) -> Option<String> {
    let section_str = section.as_str();

    // Simple string version: dep = "1.0"
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

    // Inline table or block table.
    let table = item.as_table_like()?;

    // workspace = true → OK
    if table
        .get("workspace")
        .and_then(Item::as_bool)
        .unwrap_or(false)
    {
        return None;
    }

    // path dependency without workspace → skip (local override)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn parse_item(toml_str: &str, section: DepSection, dep_name: &str) -> Item {
        let doc: syn_workspace::toml_edit::ImDocument<String> =
            syn_workspace::toml_edit::ImDocument::parse(toml_str.to_string()).unwrap();
        let table = doc.as_table();
        let section_item = match section {
            DepSection::Dependencies => table.get("dependencies"),
            DepSection::DevDependencies => table.get("dev-dependencies"),
            DepSection::BuildDependencies => table.get("build-dependencies"),
            DepSection::WorkspaceDependencies => table
                .get("workspace")
                .and_then(Item::as_table_like)
                .and_then(|t| t.get("dependencies")),
        }
        .unwrap();
        section_item
            .as_table_like()
            .unwrap()
            .get(dep_name)
            .unwrap()
            .clone()
    }

    fn parse_manifest(content: &str) -> Manifest {
        // Manifest::empty + reparse keeps tests independent of fs.
        let path = std::path::PathBuf::from("/tmp/Cargo.toml");
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("Cargo.toml");
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        drop(f);
        let m = Manifest::load(&p).unwrap();
        // sanity
        assert_eq!(m.raw(), content);
        let _ = path;
        m
    }

    // --- check_dep ---

    #[test]
    fn string_version_in_workspace() {
        let item = parse_item(
            "[dependencies]\nserde = \"1.0\"\n",
            DepSection::Dependencies,
            "serde",
        );
        let msg = check_dep("serde", &item, DepSection::Dependencies, &ws(&["serde"]));
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("use { workspace = true }"));
    }

    #[test]
    fn string_version_not_in_workspace() {
        let item = parse_item(
            "[dependencies]\nrand = \"1.0\"\n",
            DepSection::Dependencies,
            "rand",
        );
        let msg = check_dep("rand", &item, DepSection::Dependencies, &ws(&["serde"]));
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("not in [workspace.dependencies]"));
    }

    #[test]
    fn workspace_true_is_ok() {
        let item = parse_item(
            "[dependencies]\nserde = { workspace = true }\n",
            DepSection::Dependencies,
            "serde",
        );
        assert!(check_dep("serde", &item, DepSection::Dependencies, &ws(&["serde"])).is_none());
    }

    #[test]
    fn path_dep_is_ok() {
        let item = parse_item(
            "[dependencies]\nother = { path = \"../other\" }\n",
            DepSection::Dependencies,
            "other",
        );
        assert!(check_dep("other", &item, DepSection::Dependencies, &ws(&["serde"])).is_none());
    }

    #[test]
    fn table_version_in_workspace() {
        let item = parse_item(
            "[dependencies]\nserde = { version = \"1\" }\n",
            DepSection::Dependencies,
            "serde",
        );
        let msg = check_dep("serde", &item, DepSection::Dependencies, &ws(&["serde"]));
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("use { workspace = true }"));
    }

    #[test]
    fn table_version_not_in_workspace() {
        let item = parse_item(
            "[dependencies]\nserde = { version = \"1\" }\n",
            DepSection::Dependencies,
            "serde",
        );
        let msg = check_dep("serde", &item, DepSection::Dependencies, &ws(&[]));
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("not in [workspace.dependencies]"));
    }

    #[test]
    fn git_dep_in_workspace() {
        let item = parse_item(
            "[dependencies]\nbar = { git = \"https://github.com/foo/bar\" }\n",
            DepSection::Dependencies,
            "bar",
        );
        let msg = check_dep("bar", &item, DepSection::Dependencies, &ws(&["bar"]));
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("own git source"));
    }

    #[test]
    fn git_dep_not_in_workspace() {
        let item = parse_item(
            "[dependencies]\nbar = { git = \"https://github.com/foo/bar\" }\n",
            DepSection::Dependencies,
            "bar",
        );
        assert!(check_dep("bar", &item, DepSection::Dependencies, &ws(&[])).is_none());
    }

    #[test]
    fn section_appears_in_message() {
        let item = parse_item(
            "[dev-dependencies]\nfoo = \"1.0\"\n",
            DepSection::DevDependencies,
            "foo",
        );
        let msg = check_dep("foo", &item, DepSection::DevDependencies, &ws(&[])).unwrap();
        assert!(msg.contains("[dev-dependencies]"));
    }

    // --- build_rewrite_suggestion ---

    #[test]
    fn build_suggestion_produces_machine_applicable_replacement() {
        let m = parse_manifest("[package]\nname = \"a\"\n\n[dependencies]\nserde = \"1.0\"\n");
        let s = build_rewrite_suggestion(&m, DepSection::Dependencies, "serde").unwrap();
        assert_eq!(s.applicability, Applicability::MachineApplicable);
        assert_eq!(s.replacement, "serde = { workspace = true }");
        assert_eq!(
            &m.raw()[s.span.byte_start as usize..s.span.byte_end as usize],
            "serde = \"1.0\""
        );
    }

    #[test]
    fn build_suggestion_preserves_features() {
        let m = parse_manifest(
            "[dependencies]\nserde = { version = \"1\", features = [\"derive\"] }\n",
        );
        let s = build_rewrite_suggestion(&m, DepSection::Dependencies, "serde").unwrap();
        assert_eq!(
            s.replacement,
            "serde = { workspace = true, features = [\"derive\"] }"
        );
    }

    #[test]
    fn build_suggestion_returns_none_for_already_workspace() {
        // Idempotent: an already-correct dep produces no replacement (since
        // check_dep returns None for it; build_rewrite_suggestion would
        // produce the same text anyway and we short-circuit when
        // replacement == original).
        let m = parse_manifest("[dependencies]\nserde = { workspace = true }\n");
        let s = build_rewrite_suggestion(&m, DepSection::Dependencies, "serde");
        assert!(s.is_none(), "expected no rewrite, got {s:?}");
    }
}
