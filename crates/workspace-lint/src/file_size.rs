use crate::config::FileSizeConfig;
use crate::diagnostic::Diagnostic;
use crate::diagnostic::builder::{at_file, at_workspace};
use globset::{Glob, GlobSetBuilder};
use std::collections::HashMap;
use std::process::Command;
use tokei::{Config as TokeiConfig, Languages};

pub const LINT: &str = "workspace-lint::file-size";
pub const STALE_GIT_INDEX_LINT: &str = "workspace-lint::stale-git-index";

pub fn check(config: &FileSizeConfig) -> Vec<Diagnostic> {
    // Build glob matchers for each rule
    let mut builder = GlobSetBuilder::new();
    for rule in &config.rules {
        builder.add(Glob::new(&rule.glob).unwrap_or_else(|e| {
            eprintln!("invalid glob pattern '{}': {e}", rule.glob);
            std::process::exit(1);
        }));
    }
    let globset = builder.build().unwrap();

    // Use tokei to count all files (respects .gitignore)
    let mut languages = Languages::new();
    languages.get_statistics(&["."], &[], &TokeiConfig::default());

    // Aggregate code lines per file (main + embedded languages)
    let mut file_lines: HashMap<String, usize> = HashMap::new();
    for language in languages.values() {
        for report in &language.reports {
            let path = report.name.strip_prefix("./").unwrap_or(&report.name);
            let key = path.display().to_string();
            *file_lines.entry(key).or_default() += report.stats.code;
        }
        for child_reports in language.children.values() {
            for report in child_reports {
                let path = report.name.strip_prefix("./").unwrap_or(&report.name);
                let key = path.display().to_string();
                *file_lines.entry(key).or_default() += report.stats.code;
            }
        }
    }

    // Check each file against matching rules
    let mut violations: Vec<Vec<(String, usize)>> = vec![Vec::new(); config.rules.len()];

    for (path_str, code_lines) in &file_lines {
        let path = std::path::Path::new(path_str);
        let matches = globset.matches(path);
        for &rule_idx in &matches {
            if *code_lines > config.rules[rule_idx].max_code_lines {
                violations[rule_idx].push((path_str.clone(), *code_lines));
            }
        }
    }

    // One Diagnostic per offending file — gives each its own silence anchor.
    let mut diagnostics = Vec::new();
    for (rule_idx, viols) in violations.into_iter().enumerate() {
        let rule = &config.rules[rule_idx];
        for (path, code_lines) in viols {
            let d = at_file(
                LINT,
                format!(
                    "file exceeds {} code lines ({code_lines})",
                    rule.max_code_lines
                ),
                path,
            )
            .help("split #[cfg(test)] modules into separate test files")
            .help("extract related structs, enums, or trait impls into their own modules")
            .note(format!(
                "configured by [[file-size.rules]] glob = \"{}\"",
                rule.glob
            ))
            .build();
            diagnostics.push(d);
        }
    }

    diagnostics.extend(check_deleted_files());
    diagnostics
}

fn check_deleted_files() -> Vec<Diagnostic> {
    let output = Command::new("git")
        .args(["ls-files"])
        .output()
        .unwrap_or_else(|e| {
            eprintln!("failed to run git ls-files: {e}");
            std::process::exit(1);
        });

    let files = String::from_utf8_lossy(&output.stdout);
    files
        .lines()
        .filter(|path| !std::path::Path::new(path).exists())
        .map(|path| {
            at_workspace(
                STALE_GIT_INDEX_LINT,
                format!("deleted file `{path}` is still tracked by git"),
            )
            .help(format!("run `git rm {path}` to stage the removal"))
            .build()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FileSizeRule;

    fn find_violations(
        file_lines: &HashMap<String, usize>,
        config: &FileSizeConfig,
    ) -> Vec<Diagnostic> {
        let mut builder = GlobSetBuilder::new();
        for rule in &config.rules {
            builder.add(Glob::new(&rule.glob).unwrap());
        }
        let globset = builder.build().unwrap();

        let mut diags = Vec::new();
        for (path_str, code_lines) in file_lines {
            let path = std::path::Path::new(path_str);
            let matches = globset.matches(path);
            for &rule_idx in &matches {
                let rule = &config.rules[rule_idx];
                if *code_lines > rule.max_code_lines {
                    diags.push(
                        at_file(
                            LINT,
                            format!(
                                "file exceeds {} code lines ({code_lines})",
                                rule.max_code_lines
                            ),
                            path_str.clone(),
                        )
                        .build(),
                    );
                }
            }
        }
        diags
    }

    fn make_config(rules: Vec<(&str, usize)>) -> FileSizeConfig {
        FileSizeConfig {
            rules: rules
                .into_iter()
                .map(|(glob, max)| FileSizeRule {
                    glob: glob.into(),
                    max_code_lines: max,
                })
                .collect(),
        }
    }

    #[test]
    fn no_files_no_violations() {
        let config = make_config(vec![("**/*.rs", 500)]);
        let file_lines = HashMap::new();
        assert!(find_violations(&file_lines, &config).is_empty());
    }

    #[test]
    fn all_within_limit() {
        let config = make_config(vec![("**/*.rs", 500)]);
        let mut file_lines = HashMap::new();
        file_lines.insert("src/main.rs".into(), 200);
        file_lines.insert("src/lib.rs".into(), 499);
        assert!(find_violations(&file_lines, &config).is_empty());
    }

    #[test]
    fn one_over_limit() {
        let config = make_config(vec![("**/*.rs", 500)]);
        let mut file_lines = HashMap::new();
        file_lines.insert("src/main.rs".into(), 501);
        file_lines.insert("src/lib.rs".into(), 100);
        let diags = find_violations(&file_lines, &config);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint, LINT);
        assert!(diags[0].message.contains("501"));
        let span_file = diags[0].primary.as_ref().unwrap().file.to_string_lossy();
        assert_eq!(span_file, "src/main.rs");
    }

    #[test]
    fn each_violation_is_its_own_diagnostic() {
        let config = make_config(vec![("**/*.rs", 100)]);
        let mut file_lines = HashMap::new();
        file_lines.insert("a.rs".into(), 200);
        file_lines.insert("b.rs".into(), 500);
        file_lines.insert("c.rs".into(), 300);
        let diags = find_violations(&file_lines, &config);
        assert_eq!(diags.len(), 3);
    }

    #[test]
    fn multiple_rules() {
        let config = make_config(vec![("**/*.rs", 500), ("**/*.ts", 300)]);
        let mut file_lines = HashMap::new();
        file_lines.insert("src/main.rs".into(), 600);
        file_lines.insert("src/app.ts".into(), 400);
        let diags = find_violations(&file_lines, &config);
        assert_eq!(diags.len(), 2);
    }

    #[test]
    fn non_matching_glob_ignored() {
        let config = make_config(vec![("**/*.rs", 100)]);
        let mut file_lines = HashMap::new();
        file_lines.insert("script.py".into(), 9999);
        assert!(find_violations(&file_lines, &config).is_empty());
    }

    #[test]
    fn exact_limit_is_not_violation() {
        let config = make_config(vec![("**/*.rs", 500)]);
        let mut file_lines = HashMap::new();
        file_lines.insert("src/main.rs".into(), 500);
        assert!(find_violations(&file_lines, &config).is_empty());
    }
}
