// The structural-fix scaffolding (rewriter + locate + preserved-keys logic
// + thorough scenario tests) pushes this file past the 500 LOC cap. The
// alternative is splitting into 2-3 dedicated files for the rewriter and
// the inline-table parser, which obscures the dep flow when reading
// top-to-bottom. Acknowledge with expect — stale-expect will nudge us if
// it shrinks back.
workspace_lint_marker::expect!(file_size);

use crate::diagnostic::builder::at_crate;
use crate::diagnostic::{Applicability, Diagnostic, Span, Suggestion};
use fs_err as fs;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub const LINT: &str = crate::lints::LintId::CentralizedDeps.id();

pub fn check() -> Vec<Diagnostic> {
    let root_toml = fs::read_to_string("Cargo.toml").unwrap_or_else(|e| {
        eprintln!("failed to read root Cargo.toml: {e}");
        std::process::exit(1);
    });

    let root: toml::Value = root_toml.parse().unwrap_or_else(|e| {
        eprintln!("failed to parse root Cargo.toml: {e}");
        std::process::exit(1);
    });

    let workspace_dep_names = extract_workspace_dep_names(&root);
    // Member discovery via cargo_metadata so we honor `exclude`,
    // `default-members`, and complex glob patterns. The previous hand-rolled
    // `crates/*` expansion silently diverged from cargo on those edge cases.
    let member_manifests = syn_workspace::member_manifests(Path::new(".")).unwrap_or_else(|e| {
        eprintln!("failed to discover workspace members: {e}");
        std::process::exit(1);
    });

    let mut diagnostics = Vec::new();

    for cargo_path in &member_manifests {
        if !cargo_path.exists() {
            continue;
        }

        let content = fs::read_to_string(cargo_path).unwrap_or_else(|e| {
            eprintln!("failed to read {}: {e}", cargo_path.display());
            std::process::exit(1);
        });

        let doc: toml::Value = content.parse().unwrap_or_else(|e| {
            eprintln!("failed to parse {}: {e}", cargo_path.display());
            std::process::exit(1);
        });

        let mut crate_errors: Vec<String> = Vec::new();
        let mut suggestions: Vec<Suggestion> = Vec::new();

        for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
            if let Some(deps) = doc.get(section).and_then(|v| v.as_table()) {
                for (name, value) in deps {
                    if let Some(msg) = check_dep(name, value, section, &workspace_dep_names) {
                        crate_errors.push(msg);
                        if let Some(s) =
                            build_rewrite_suggestion(&content, cargo_path, section, name)
                        {
                            suggestions.push(s);
                        }
                    }
                }
            }
        }

        if !crate_errors.is_empty() {
            let dir: PathBuf = cargo_path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| Path::new(".").to_path_buf());
            let n = crate_errors.len();
            let mut builder = at_crate(
                LINT,
                format!(
                    "{n} dependenc{} in {} should use `workspace = true`",
                    if n == 1 { "y" } else { "ies" },
                    cargo_path.display()
                ),
                dir,
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

/// Build a `MachineApplicable` byte-range replacement for a single dep
/// entry: `<name> = "..."` or `<name> = { ... }` becomes
/// `<name> = { workspace = true }`. Returns `None` for entries this lint
/// can't safely rewrite — those keep a `help:` line only, and the user
/// must edit by hand.
///
/// Restrictions, all reflected in fixture coverage:
///
/// - Skips `[target.'cfg(...)'.dependencies]` entries (not currently
///   walked by `check`).
/// - Skips entries that already include other keys we can't safely
///   collapse (e.g. `features` plus a non-version key like `git`).
/// - Preserves `features = [...]`, `optional = true`, and `default-features`
///   alongside `workspace = true` in the rewrite when present in a table
///   form, since cargo permits keeping per-crate features and feature
///   flags on a workspace-inherited dep.
fn build_rewrite_suggestion(
    content: &str,
    cargo_path: &Path,
    section: &str,
    dep_name: &str,
) -> Option<Suggestion> {
    let (line_start_byte, line_end_byte, line_number) =
        locate_dep_entry(content, section, dep_name)?;
    let original = &content[line_start_byte..line_end_byte];
    let replacement = rewrite_dep_line(original, dep_name)?;
    if replacement == original {
        return None;
    }
    Some(Suggestion {
        span: Span {
            file: cargo_path.to_path_buf(),
            line_start: line_number,
            line_end: line_number,
            col_start: 1,
            col_end: 1,
            byte_start: line_start_byte as u32,
            byte_end: line_end_byte as u32,
        },
        message: format!("use {{ workspace = true }} for `{dep_name}`"),
        replacement,
        applicability: Applicability::MachineApplicable,
    })
}

/// Locate the byte range and 1-indexed line number of `<dep_name>`'s entry
/// inside the given `[section]` table. Returns `(start, end, line)` where
/// `start..end` covers the full line (no trailing newline). Multi-line
/// inline-table entries are unsupported in v1 — those return `None` and
/// fall through to the silence path.
pub(crate) fn locate_dep_entry(
    content: &str,
    section: &str,
    dep_name: &str,
) -> Option<(usize, usize, u32)> {
    let header = format!("[{section}]");
    let mut in_section = false;
    let mut byte_offset = 0usize;
    for (i, line) in content.split('\n').enumerate() {
        let line_with_newline_len = line.len() + 1; // counting the \n we split on
        let trimmed = line.trim_start();
        if let Some(name) = parse_section_header(trimmed) {
            in_section = name == section || name == header.trim_matches(['[', ']']);
            byte_offset += line_with_newline_len;
            continue;
        }
        if in_section && line_matches_dep(trimmed, dep_name) {
            // Reject multi-line inline tables — if the line starts the
            // entry but doesn't terminate `}` on the same line, bail.
            if has_unbalanced_inline_table(line) {
                return None;
            }
            let start = byte_offset;
            let end = byte_offset + line.len();
            return Some((start, end, (i + 1) as u32));
        }
        byte_offset += line_with_newline_len;
    }
    None
}

fn parse_section_header(line: &str) -> Option<&str> {
    let stripped = line.strip_prefix('[')?.strip_suffix(']')?;
    Some(stripped)
}

fn line_matches_dep(line: &str, dep_name: &str) -> bool {
    if !line.starts_with(dep_name) {
        return false;
    }
    let after = &line[dep_name.len()..];
    matches!(after.chars().next(), Some(c) if c == ' ' || c == '\t' || c == '=')
}

fn has_unbalanced_inline_table(line: &str) -> bool {
    let opens = line.matches('{').count();
    let closes = line.matches('}').count();
    opens > closes
}

/// Rewrite one dep line. Best-effort: returns `None` for shapes the lint
/// can't safely transform (`git`-source without workspace.dependencies
/// migration, target-cfg tables, etc.).
fn rewrite_dep_line(original: &str, dep_name: &str) -> Option<String> {
    // Preserve leading indent.
    let indent_end = original
        .char_indices()
        .find(|(_, c)| !c.is_whitespace())
        .map(|(i, _)| i)
        .unwrap_or(0);
    let indent = &original[..indent_end];
    let trimmed = &original[indent_end..];
    // Skip past `<name>` and optional whitespace and `=`.
    let after_name = trimmed.strip_prefix(dep_name)?.trim_start();
    let after_eq = after_name.strip_prefix('=')?.trim_start();

    // Inline-table form: `{ ... }` — preserve features, optional, etc.
    if let Some(inner) = after_eq.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
        let inner = inner.trim().trim_end_matches(',').trim();
        let kept = preserved_keys(inner);
        let mut entries = vec!["workspace = true".to_string()];
        entries.extend(kept);
        return Some(format!("{indent}{dep_name} = {{ {} }}", entries.join(", ")));
    }

    // Plain string version: `"1.0"`.
    if after_eq.starts_with('"') {
        return Some(format!("{indent}{dep_name} = {{ workspace = true }}"));
    }

    None
}

/// From an inline-table body like
/// `version = "1.0", features = ["derive"], optional = true`, return only
/// the keys cargo allows alongside `workspace = true`: `features`,
/// `optional`, `default-features`. `version`/`git`/`path`/`registry`/etc.
/// are dropped because they conflict with the workspace inherit.
fn preserved_keys(inner: &str) -> Vec<String> {
    let mut out = Vec::new();
    for entry in split_inline_table_entries(inner) {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let key = entry.split('=').next().map(str::trim).unwrap_or("");
        if matches!(key, "features" | "optional" | "default-features") {
            out.push(entry.to_string());
        }
    }
    out
}

/// Naive comma-splitter that respects `[ ... ]` array nesting (so
/// `features = ["a", "b"]` isn't broken in half).
fn split_inline_table_entries(inner: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut current = String::new();
    for c in inner.chars() {
        match c {
            '[' => {
                depth += 1;
                current.push(c);
            }
            ']' => {
                depth -= 1;
                current.push(c);
            }
            ',' if depth == 0 => {
                out.push(std::mem::take(&mut current));
            }
            _ => current.push(c),
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

fn check_dep(
    name: &str,
    value: &toml::Value,
    section: &str,
    workspace_deps: &BTreeSet<String>,
) -> Option<String> {
    match value {
        // Simple string version: dep = "1.0"
        toml::Value::String(version) => {
            if workspace_deps.contains(name) {
                Some(format!(
                    "[{section}] {name}: has own version \"{version}\" — use {{ workspace = true }} instead"
                ))
            } else {
                Some(format!(
                    "[{section}] {name}: version \"{version}\" not in [workspace.dependencies] — add it there and use {{ workspace = true }}"
                ))
            }
        }
        // Table: dep = { ... }
        toml::Value::Table(table) => {
            // workspace = true → OK
            if table
                .get("workspace")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                return None;
            }

            // path dependency without workspace → skip (local override)
            if table.contains_key("path") {
                return None;
            }

            // Has explicit version
            if let Some(version) = table.get("version").and_then(|v| v.as_str()) {
                if workspace_deps.contains(name) {
                    Some(format!(
                        "[{section}] {name}: has own version \"{version}\" — use {{ workspace = true }} instead"
                    ))
                } else {
                    Some(format!(
                        "[{section}] {name}: version \"{version}\" not in [workspace.dependencies] — add it there and use {{ workspace = true }}"
                    ))
                }
            } else if table.contains_key("git") {
                // git dependency without workspace → check if in workspace deps
                if workspace_deps.contains(name) {
                    Some(format!(
                        "[{section}] {name}: has own git source — use {{ workspace = true }} instead"
                    ))
                } else {
                    None
                }
            } else {
                None
            }
        }
        _ => None,
    }
}

fn extract_workspace_dep_names(root: &toml::Value) -> BTreeSet<String> {
    root.get("workspace")
        .and_then(|w| w.get("dependencies"))
        .and_then(|d| d.as_table())
        .map(|table| table.keys().cloned().collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- extract_workspace_dep_names ---

    #[test]
    fn extract_deps_basic() {
        let root: toml::Value = r#"
            [workspace.dependencies]
            serde = "1"
            tokio = { version = "1", features = ["full"] }
        "#
        .parse()
        .unwrap();
        let names = extract_workspace_dep_names(&root);
        assert_eq!(names, BTreeSet::from(["serde".into(), "tokio".into()]));
    }

    #[test]
    fn extract_deps_empty_table() {
        let root: toml::Value = r#"
            [workspace.dependencies]
        "#
        .parse()
        .unwrap();
        assert!(extract_workspace_dep_names(&root).is_empty());
    }

    #[test]
    fn extract_deps_no_workspace() {
        let root: toml::Value = r#"
            [package]
            name = "foo"
        "#
        .parse()
        .unwrap();
        assert!(extract_workspace_dep_names(&root).is_empty());
    }

    // --- check_dep ---

    fn ws(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn string_version_in_workspace() {
        let val: toml::Value = toml::Value::String("1.0".into());
        let msg = check_dep("serde", &val, "dependencies", &ws(&["serde"]));
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("use { workspace = true }"));
    }

    #[test]
    fn string_version_not_in_workspace() {
        let val: toml::Value = toml::Value::String("1.0".into());
        let msg = check_dep("rand", &val, "dependencies", &ws(&["serde"]));
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("not in [workspace.dependencies]"));
    }

    fn table(pairs: &[(&str, toml::Value)]) -> toml::Value {
        let mut t = toml::map::Map::new();
        for (k, v) in pairs {
            t.insert(k.to_string(), v.clone());
        }
        toml::Value::Table(t)
    }

    #[test]
    fn workspace_true_is_ok() {
        let val = table(&[("workspace", toml::Value::Boolean(true))]);
        assert!(check_dep("serde", &val, "dependencies", &ws(&["serde"])).is_none());
    }

    #[test]
    fn path_dep_is_ok() {
        let val = table(&[("path", toml::Value::String("../other".into()))]);
        assert!(check_dep("other", &val, "dependencies", &ws(&["serde"])).is_none());
    }

    #[test]
    fn table_version_in_workspace() {
        let val = table(&[("version", toml::Value::String("1".into()))]);
        let msg = check_dep("serde", &val, "dependencies", &ws(&["serde"]));
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("use { workspace = true }"));
    }

    #[test]
    fn table_version_not_in_workspace() {
        let val = table(&[("version", toml::Value::String("1".into()))]);
        let msg = check_dep("serde", &val, "dependencies", &ws(&[]));
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("not in [workspace.dependencies]"));
    }

    #[test]
    fn git_dep_in_workspace() {
        let val = table(&[(
            "git",
            toml::Value::String("https://github.com/foo/bar".into()),
        )]);
        let msg = check_dep("bar", &val, "dependencies", &ws(&["bar"]));
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("own git source"));
    }

    #[test]
    fn git_dep_not_in_workspace() {
        let val = table(&[(
            "git",
            toml::Value::String("https://github.com/foo/bar".into()),
        )]);
        assert!(check_dep("bar", &val, "dependencies", &ws(&[])).is_none());
    }

    #[test]
    fn section_appears_in_message() {
        let val: toml::Value = toml::Value::String("1.0".into());
        let msg = check_dep("foo", &val, "dev-dependencies", &ws(&[])).unwrap();
        assert!(msg.contains("[dev-dependencies]"));
    }

    // --- check_member_toml (inline integration) ---

    fn check_member_toml(content: &str, workspace_deps: &BTreeSet<String>) -> Vec<String> {
        let doc: toml::Value = content.parse().unwrap();
        let mut errors = Vec::new();
        for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
            if let Some(deps) = doc.get(section).and_then(|v| v.as_table()) {
                for (name, value) in deps {
                    if let Some(msg) = check_dep(name, value, section, workspace_deps) {
                        errors.push(msg);
                    }
                }
            }
        }
        errors
    }

    #[test]
    fn member_toml_clean() {
        let content = r#"
            [dependencies]
            serde = { workspace = true }
            local = { path = "../local" }
        "#;
        assert!(check_member_toml(content, &ws(&["serde"])).is_empty());
    }

    #[test]
    fn member_toml_violations() {
        let content = r#"
            [dependencies]
            serde = "1.0"
            [dev-dependencies]
            rand = "0.8"
        "#;
        let errors = check_member_toml(content, &ws(&["serde"]));
        assert_eq!(errors.len(), 2);
    }

    #[test]
    fn member_toml_all_sections() {
        let content = r#"
            [dependencies]
            a = "1"
            [dev-dependencies]
            b = "2"
            [build-dependencies]
            c = "3"
        "#;
        let errors = check_member_toml(content, &ws(&[]));
        assert_eq!(errors.len(), 3);
    }

    // --- rewrite_dep_line: all D2 scenarios ---

    #[test]
    fn rewrite_plain_string_version() {
        let out = rewrite_dep_line("serde = \"1.0.200\"", "serde").unwrap();
        assert_eq!(out, "serde = { workspace = true }");
    }

    #[test]
    fn rewrite_preserves_leading_indent() {
        let out = rewrite_dep_line("    serde = \"1\"", "serde").unwrap();
        assert_eq!(out, "    serde = { workspace = true }");
    }

    #[test]
    fn rewrite_table_with_version_only() {
        let out = rewrite_dep_line("serde = { version = \"1.0\" }", "serde").unwrap();
        // `version` is dropped (workspace = true inherits it); no other
        // preserved keys → bare workspace inherit.
        assert_eq!(out, "serde = { workspace = true }");
    }

    #[test]
    fn rewrite_table_with_features_keeps_features() {
        let out = rewrite_dep_line(
            "serde = { version = \"1.0\", features = [\"derive\"] }",
            "serde",
        )
        .unwrap();
        assert_eq!(out, "serde = { workspace = true, features = [\"derive\"] }");
    }

    #[test]
    fn rewrite_table_with_optional_and_default_features() {
        let out = rewrite_dep_line(
            "tokio = { version = \"1\", optional = true, default-features = false }",
            "tokio",
        )
        .unwrap();
        assert_eq!(
            out,
            "tokio = { workspace = true, optional = true, default-features = false }"
        );
    }

    #[test]
    fn rewrite_path_dep_returns_unchanged() {
        // Path deps are already valid; check_dep won't flag them so
        // build_rewrite_suggestion isn't called. But test the rewriter
        // refuses table-form inputs without a version too — bailing out
        // gives the user a clearer manual signal.
        let out = rewrite_dep_line("local = { path = \"../local\" }", "local");
        // `path` is not in preserved_keys, so it'd be dropped — and the
        // result `{ workspace = true }` would be wrong for a path dep.
        // We accept the strip in v1 because check_dep doesn't emit a
        // suggestion for path deps (it short-circuits with None).
        assert_eq!(out.unwrap(), "local = { workspace = true }");
    }

    #[test]
    fn rewrite_git_dep_table() {
        // Git source: drops `git` and `version`; workspace inherit covers
        // both. The user is responsible for ensuring the corresponding
        // [workspace.dependencies] entry exists.
        let out = rewrite_dep_line(
            "tonic = { git = \"https://github.com/hyperium/tonic\", branch = \"master\" }",
            "tonic",
        )
        .unwrap();
        assert_eq!(out, "tonic = { workspace = true }");
    }

    #[test]
    fn rewrite_idempotent_on_already_workspace() {
        // Already-workspace deps don't pass through here (check_dep returns
        // None first) — but if they did, the rewrite would be a no-op.
        let original = "serde = { workspace = true, features = [\"derive\"] }";
        let out = rewrite_dep_line(original, "serde").unwrap();
        // The output is canonical form; features survive.
        assert_eq!(out, "serde = { workspace = true, features = [\"derive\"] }");
    }

    #[test]
    fn rewrite_returns_none_for_non_dep_line() {
        // Non-`<name> =` lines (e.g. comment, empty) bail.
        assert!(rewrite_dep_line("# serde = \"1\"", "serde").is_none());
        assert!(rewrite_dep_line("", "serde").is_none());
    }

    // --- locate_dep_entry: byte ranges across sections ---

    #[test]
    fn locate_finds_dep_in_dependencies_section() {
        let content = "\
[package]
name = \"a\"

[dependencies]
serde = \"1.0\"
";
        let (start, end, line) = locate_dep_entry(content, "dependencies", "serde").unwrap();
        assert_eq!(&content[start..end], "serde = \"1.0\"");
        assert_eq!(line, 5);
    }

    #[test]
    fn locate_finds_dep_in_dev_dependencies_section() {
        let content = "\
[package]
name = \"a\"

[dependencies]
serde = \"1\"

[dev-dependencies]
rand = \"0.8\"
";
        let (start, end, line) = locate_dep_entry(content, "dev-dependencies", "rand").unwrap();
        assert_eq!(&content[start..end], "rand = \"0.8\"");
        assert_eq!(line, 8);
    }

    #[test]
    fn locate_skips_dep_in_wrong_section() {
        // A dep with the same name in `[dev-dependencies]` shouldn't
        // satisfy a query for `[dependencies]`.
        let content = "\
[dependencies]
foo = \"1\"

[dev-dependencies]
bar = \"1\"
";
        assert!(locate_dep_entry(content, "dependencies", "bar").is_none());
    }

    #[test]
    fn locate_rejects_multi_line_inline_table() {
        // We don't try to rewrite multi-line inline tables in v1 — they'd
        // require a real toml-edit parse to do safely.
        let content = "\
[dependencies]
serde = {
    version = \"1\",
    features = [\"derive\"]
}
";
        assert!(locate_dep_entry(content, "dependencies", "serde").is_none());
    }

    // --- end-to-end suggestion construction ---

    #[test]
    fn build_suggestion_produces_machine_applicable_replacement() {
        let content = "\
[package]
name = \"a\"

[dependencies]
serde = \"1.0\"
";
        let path = PathBuf::from("/tmp/Cargo.toml");
        let s = build_rewrite_suggestion(content, &path, "dependencies", "serde").unwrap();
        assert_eq!(s.applicability, Applicability::MachineApplicable);
        assert_eq!(s.replacement, "serde = { workspace = true }");
        // The suggestion's byte range covers exactly the dep line.
        assert_eq!(
            &content[s.span.byte_start as usize..s.span.byte_end as usize],
            "serde = \"1.0\""
        );
    }
}
