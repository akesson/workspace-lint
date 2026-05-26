use crate::{Issue, workspace};
use fs_err as fs;
use std::collections::BTreeSet;

pub fn check() -> Vec<Issue> {
    let root_toml = fs::read_to_string("Cargo.toml").unwrap_or_else(|e| {
        eprintln!("failed to read root Cargo.toml: {e}");
        std::process::exit(1);
    });

    let root: toml::Value = root_toml.parse().unwrap_or_else(|e| {
        eprintln!("failed to parse root Cargo.toml: {e}");
        std::process::exit(1);
    });

    let workspace_dep_names = extract_workspace_dep_names(&root);
    let member_patterns = workspace::extract_member_patterns(&root);
    let member_dirs = workspace::expand_member_patterns(&member_patterns);

    let mut issues = Vec::new();

    for dir in &member_dirs {
        let cargo_path = dir.join("Cargo.toml");
        if !cargo_path.exists() {
            continue;
        }

        let content = fs::read_to_string(&cargo_path).unwrap_or_else(|e| {
            eprintln!("failed to read {}: {e}", cargo_path.display());
            std::process::exit(1);
        });

        let doc: toml::Value = content.parse().unwrap_or_else(|e| {
            eprintln!("failed to parse {}: {e}", cargo_path.display());
            std::process::exit(1);
        });

        let mut crate_errors: Vec<String> = Vec::new();

        for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
            if let Some(deps) = doc.get(section).and_then(|v| v.as_table()) {
                for (name, value) in deps {
                    if let Some(msg) = check_dep(name, value, section, &workspace_dep_names) {
                        crate_errors.push(msg);
                    }
                }
            }
        }

        if !crate_errors.is_empty() {
            issues.push(Issue {
                title: format!("Fix workspace deps in {}", cargo_path.display()),
                details: crate_errors,
            });
        }
    }

    issues
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
}
