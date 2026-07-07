// These tests call the production `find_violations` directly (the tokei walk in
// `check`/`collect_file_lines` is exercised end-to-end by tests/cases/file-size).
use super::*;
use std::collections::HashMap;

fn run(file_lines: &HashMap<String, usize>, config: &FileSizeConfig) -> Vec<Diagnostic> {
    find_violations(file_lines, &config.rules)
}

fn make_config(rules: Vec<(&str, usize)>) -> FileSizeConfig {
    FileSizeConfig {
        rules: rules
            .into_iter()
            .map(|(glob, max)| FileSizeRule {
                glob: GlobPattern::new(glob).unwrap(),
                max_code_lines: max,
            })
            .collect(),
    }
}

#[test]
fn no_files_no_violations() {
    let config = make_config(vec![("**/*.rs", 500)]);
    let file_lines = HashMap::new();
    assert!(run(&file_lines, &config).is_empty());
}

#[test]
fn all_within_limit() {
    let config = make_config(vec![("**/*.rs", 500)]);
    let mut file_lines = HashMap::new();
    file_lines.insert("src/main.rs".into(), 200);
    file_lines.insert("src/lib.rs".into(), 499);
    assert!(run(&file_lines, &config).is_empty());
}

#[test]
fn one_over_limit() {
    let config = make_config(vec![("**/*.rs", 500)]);
    let mut file_lines = HashMap::new();
    file_lines.insert("src/main.rs".into(), 501);
    file_lines.insert("src/lib.rs".into(), 100);
    let diags = run(&file_lines, &config);
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
    let diags = run(&file_lines, &config);
    assert_eq!(diags.len(), 3);
}

#[test]
fn multiple_rules() {
    let config = make_config(vec![("**/*.rs", 500), ("**/*.ts", 300)]);
    let mut file_lines = HashMap::new();
    file_lines.insert("src/main.rs".into(), 600);
    file_lines.insert("src/app.ts".into(), 400);
    let diags = run(&file_lines, &config);
    assert_eq!(diags.len(), 2);
}

#[test]
fn non_matching_glob_ignored() {
    let config = make_config(vec![("**/*.rs", 100)]);
    let mut file_lines = HashMap::new();
    file_lines.insert("script.py".into(), 9999);
    assert!(run(&file_lines, &config).is_empty());
}

#[test]
fn exact_limit_is_not_violation() {
    let config = make_config(vec![("**/*.rs", 500)]);
    let mut file_lines = HashMap::new();
    file_lines.insert("src/main.rs".into(), 500);
    assert!(run(&file_lines, &config).is_empty());
}
