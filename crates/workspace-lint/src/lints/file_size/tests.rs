use super::*;
use crate::diagnostic::Diagnostic;
use crate::diagnostic::builder::at_file;
use globset::{Glob, GlobSetBuilder};
use std::collections::HashMap;

fn find_violations(
    file_lines: &HashMap<String, usize>,
    config: &FileSizeConfig,
) -> Vec<Diagnostic> {
    let mut builder = GlobSetBuilder::new();
    for rule in &config.rules {
        builder.add(Glob::new(&rule.glob).unwrap());
    }
    let globset = builder.build().unwrap();

    let lint_id = LintId::FileSize.id();
    let mut diags = Vec::new();
    for (path_str, code_lines) in file_lines {
        let path = std::path::Path::new(path_str);
        let matches = globset.matches(path);
        for &rule_idx in &matches {
            let rule = &config.rules[rule_idx];
            if *code_lines > rule.max_code_lines {
                diags.push(
                    at_file(
                        lint_id,
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
    assert_eq!(diags[0].lint, LintId::FileSize.id());
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
