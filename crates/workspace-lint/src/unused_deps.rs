use crate::config::UnusedDepsConfig;
use crate::{Issue, workspace};
use fs_err as fs;
use ignore::WalkBuilder;
use regex::Regex;
use std::collections::BTreeMap;
use std::path::Path;

pub fn check(config: &UnusedDepsConfig) -> Vec<Issue> {
    let root_toml = fs::read_to_string("Cargo.toml").unwrap_or_else(|e| {
        eprintln!("failed to read root Cargo.toml: {e}");
        std::process::exit(1);
    });

    let root: toml::Value = root_toml.parse().unwrap_or_else(|e| {
        eprintln!("failed to parse root Cargo.toml: {e}");
        std::process::exit(1);
    });

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

        // Collect deps: normalized_name -> vec of "[section] original_name"
        let mut deps: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
            if let Some(table) = doc.get(section).and_then(|v| v.as_table()) {
                for name in table.keys() {
                    if config.ignore.iter().any(|i| i == name) {
                        continue;
                    }
                    let normalized = name.replace('-', "_");
                    deps.entry(normalized)
                        .or_default()
                        .push(format!("[{section}] {name}"));
                }
            }
        }

        if deps.is_empty() {
            continue;
        }

        // Walk .rs files and check for usage
        let source_contents = collect_rs_sources(dir);

        let unused: Vec<String> = deps
            .into_iter()
            .filter(|(normalized, _)| {
                let pattern = format!(r"\b{}\b", regex::escape(normalized));
                let re = Regex::new(&pattern).expect("valid regex");
                !source_contents.iter().any(|src| re.is_match(src))
            })
            .flat_map(|(_, labels)| labels)
            .collect();

        if !unused.is_empty() {
            let mut details: Vec<String> = unused;
            details.push(String::new());
            details.push(
                "Note: proc-macro crates and build.rs-generated code may cause false positives."
                    .into(),
            );
            details
                .push("Try removing the flagged deps and run `cargo build --all-targets`.".into());
            details.push(
                "If it breaks, add the dep to [unused-deps] ignore in your workspace-lint config."
                    .into(),
            );

            issues.push(Issue {
                title: format!("Possibly unused deps in {}", cargo_path.display()),
                details,
            });
        }
    }

    issues
}

#[cfg(test)]
fn collect_deps_from_toml(doc: &toml::Value, ignore: &[String]) -> BTreeMap<String, Vec<String>> {
    let mut deps: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(table) = doc.get(section).and_then(|v| v.as_table()) {
            for name in table.keys() {
                if ignore.iter().any(|i| i == name) {
                    continue;
                }
                let normalized = name.replace('-', "_");
                deps.entry(normalized)
                    .or_default()
                    .push(format!("[{section}] {name}"));
            }
        }
    }
    deps
}

#[cfg(test)]
fn find_unused_deps(
    deps: BTreeMap<String, Vec<String>>,
    source_contents: &[String],
) -> Vec<String> {
    deps.into_iter()
        .filter(|(normalized, _)| {
            let pattern = format!(r"\b{}\b", regex::escape(normalized));
            let re = Regex::new(&pattern).expect("valid regex");
            !source_contents.iter().any(|src| re.is_match(src))
        })
        .flat_map(|(_, labels)| labels)
        .collect()
}

fn collect_rs_sources(dir: &Path) -> Vec<String> {
    let mut sources = Vec::new();

    for entry in WalkBuilder::new(dir)
        .hidden(false)
        .build()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "rs")
            && let Ok(content) = fs::read_to_string(path)
        {
            sources.push(content);
        }
    }

    sources
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // --- collect_deps_from_toml ---

    #[test]
    fn collect_deps_basic() {
        let doc: toml::Value = r#"
            [dependencies]
            serde = "1"
            tokio = { workspace = true }
        "#
        .parse()
        .unwrap();
        let deps = collect_deps_from_toml(&doc, &[]);
        assert!(deps.contains_key("serde"));
        assert!(deps.contains_key("tokio"));
    }

    #[test]
    fn collect_deps_normalizes_hyphens() {
        let doc: toml::Value = r#"
            [dependencies]
            my-crate = "1"
        "#
        .parse()
        .unwrap();
        let deps = collect_deps_from_toml(&doc, &[]);
        assert!(deps.contains_key("my_crate"));
    }

    #[test]
    fn collect_deps_respects_ignore() {
        let doc: toml::Value = r#"
            [dependencies]
            serde = "1"
            prost = "0.12"
        "#
        .parse()
        .unwrap();
        let deps = collect_deps_from_toml(&doc, &["prost".into()]);
        assert!(deps.contains_key("serde"));
        assert!(!deps.contains_key("prost"));
    }

    #[test]
    fn collect_deps_all_sections() {
        let doc: toml::Value = r#"
            [dependencies]
            a = "1"
            [dev-dependencies]
            b = "1"
            [build-dependencies]
            c = "1"
        "#
        .parse()
        .unwrap();
        let deps = collect_deps_from_toml(&doc, &[]);
        assert_eq!(deps.len(), 3);
        assert!(deps["a"][0].contains("[dependencies]"));
        assert!(deps["b"][0].contains("[dev-dependencies]"));
        assert!(deps["c"][0].contains("[build-dependencies]"));
    }

    // --- find_unused_deps ---

    #[test]
    fn find_unused_all_used() {
        let mut deps = BTreeMap::new();
        deps.insert("serde".into(), vec!["[dependencies] serde".into()]);
        let sources = vec!["use serde::Deserialize;".into()];
        assert!(find_unused_deps(deps, &sources).is_empty());
    }

    #[test]
    fn find_unused_none_used() {
        let mut deps = BTreeMap::new();
        deps.insert("serde".into(), vec!["[dependencies] serde".into()]);
        let sources = vec!["fn main() {}".into()];
        let unused = find_unused_deps(deps, &sources);
        assert_eq!(unused, vec!["[dependencies] serde"]);
    }

    #[test]
    fn find_unused_partial() {
        let mut deps = BTreeMap::new();
        deps.insert("serde".into(), vec!["[dependencies] serde".into()]);
        deps.insert("rand".into(), vec!["[dependencies] rand".into()]);
        let sources = vec!["use serde::Serialize;".into()];
        let unused = find_unused_deps(deps, &sources);
        assert_eq!(unused, vec!["[dependencies] rand"]);
    }

    #[test]
    fn find_unused_word_boundary() {
        let mut deps = BTreeMap::new();
        deps.insert("log".into(), vec!["[dependencies] log".into()]);
        // "dialog" contains "log" but not at word boundary
        let sources = vec!["let dialog = open_dialog();".into()];
        let unused = find_unused_deps(deps, &sources);
        assert_eq!(unused, vec!["[dependencies] log"]);
    }

    #[test]
    fn find_unused_multiple_sources() {
        let mut deps = BTreeMap::new();
        deps.insert("serde".into(), vec!["[dependencies] serde".into()]);
        let sources = vec!["fn foo() {}".into(), "use serde::Deserialize;".into()];
        assert!(find_unused_deps(deps, &sources).is_empty());
    }

    // --- collect_rs_sources (tempdir) ---

    #[test]
    fn collect_rs_sources_finds_files() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("lib.rs"), "use serde;").unwrap();
        std::fs::write(src.join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(src.join("readme.txt"), "not rust").unwrap();

        let sources = collect_rs_sources(tmp.path());
        assert_eq!(sources.len(), 2);
    }

    #[test]
    fn collect_rs_sources_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let sources = collect_rs_sources(tmp.path());
        assert!(sources.is_empty());
    }
}
