use crate::Issue;
use crate::config::FileSizeConfig;
use globset::{Glob, GlobSetBuilder};
use std::collections::HashMap;
use std::process::Command;
use tokei::{Config as TokeiConfig, Languages};

pub fn check(config: &FileSizeConfig) -> Vec<Issue> {
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

    // Build issues per rule
    let mut issues = Vec::new();
    for (rule_idx, mut viols) in violations.into_iter().enumerate() {
        if viols.is_empty() {
            continue;
        }
        viols.sort_by_key(|v| std::cmp::Reverse(v.1));

        let rule = &config.rules[rule_idx];
        let mut details: Vec<String> = viols
            .iter()
            .map(|(path, count)| format!("{path}: {count} code lines"))
            .collect();

        details.push(String::new());
        details.push("Suggestions:".to_string());
        details.push("- Move #[cfg(test)] modules to separate test files".to_string());
        details.push(
            "- Extract related structs, enums, or trait impls into their own modules".to_string(),
        );

        issues.push(Issue {
            title: format!(
                "Reduce files matching '{}' to ≤ {} code lines",
                rule.glob, rule.max_code_lines
            ),
            details,
        });
    }

    // Check for deleted files still tracked by git
    issues.extend(check_deleted_files());

    issues
}

fn check_deleted_files() -> Vec<Issue> {
    let output = Command::new("git")
        .args(["ls-files"])
        .output()
        .unwrap_or_else(|e| {
            eprintln!("failed to run git ls-files: {e}");
            std::process::exit(1);
        });

    let files = String::from_utf8_lossy(&output.stdout);
    let deleted: Vec<String> = files
        .lines()
        .filter(|path| !std::path::Path::new(path).exists())
        .map(|s| s.to_string())
        .collect();

    if deleted.is_empty() {
        Vec::new()
    } else {
        vec![Issue {
            title: "Stage deleted files with `git rm`".to_string(),
            details: deleted,
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FileSizeRule;

    fn find_violations(file_lines: &HashMap<String, usize>, config: &FileSizeConfig) -> Vec<Issue> {
        let mut builder = GlobSetBuilder::new();
        for rule in &config.rules {
            builder.add(Glob::new(&rule.glob).unwrap());
        }
        let globset = builder.build().unwrap();

        let mut violations: Vec<Vec<(String, usize)>> = vec![Vec::new(); config.rules.len()];

        for (path_str, code_lines) in file_lines {
            let path = std::path::Path::new(path_str);
            let matches = globset.matches(path);
            for &rule_idx in &matches {
                if *code_lines > config.rules[rule_idx].max_code_lines {
                    violations[rule_idx].push((path_str.clone(), *code_lines));
                }
            }
        }

        let mut issues = Vec::new();
        for (rule_idx, mut viols) in violations.into_iter().enumerate() {
            if viols.is_empty() {
                continue;
            }
            viols.sort_by_key(|v| std::cmp::Reverse(v.1));

            let rule = &config.rules[rule_idx];
            let details: Vec<String> = viols
                .iter()
                .map(|(path, count)| format!("{path}: {count} code lines"))
                .collect();

            issues.push(Issue {
                title: format!(
                    "Reduce files matching '{}' to ≤ {} code lines",
                    rule.glob, rule.max_code_lines
                ),
                details,
            });
        }

        issues
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
        let issues = find_violations(&file_lines, &config);
        assert_eq!(issues.len(), 1);
        assert!(issues[0].details[0].contains("src/main.rs"));
        assert!(issues[0].details[0].contains("501"));
    }

    #[test]
    fn sorted_descending() {
        let config = make_config(vec![("**/*.rs", 100)]);
        let mut file_lines = HashMap::new();
        file_lines.insert("a.rs".into(), 200);
        file_lines.insert("b.rs".into(), 500);
        file_lines.insert("c.rs".into(), 300);
        let issues = find_violations(&file_lines, &config);
        assert_eq!(issues.len(), 1);
        assert!(issues[0].details[0].contains("500"));
        assert!(issues[0].details[1].contains("300"));
        assert!(issues[0].details[2].contains("200"));
    }

    #[test]
    fn multiple_rules() {
        let config = make_config(vec![("**/*.rs", 500), ("**/*.ts", 300)]);
        let mut file_lines = HashMap::new();
        file_lines.insert("src/main.rs".into(), 600);
        file_lines.insert("src/app.ts".into(), 400);
        let issues = find_violations(&file_lines, &config);
        assert_eq!(issues.len(), 2);
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
