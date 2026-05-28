use super::*;
use crate::diagnostic::Diagnostic;
use crate::diagnostic::builder::at_crate;
use tempfile::TempDir;

fn find_crate_violations(
    dir_line_counts: &[(String, usize)],
    rule: &CrateSizeRule,
) -> Vec<Diagnostic> {
    let lint_id = LintId::CrateSize.id();
    dir_line_counts
        .iter()
        .filter(|(_, count)| *count > rule.max_code_lines)
        .map(|(dir, count)| {
            at_crate(
                lint_id,
                format!("crate exceeds {} code lines ({count})", rule.max_code_lines),
                dir.clone(),
            )
            .build()
        })
        .collect()
}

fn rule(glob: &str, max: usize) -> CrateSizeRule {
    CrateSizeRule {
        glob: glob.into(),
        max_code_lines: max,
        include: None,
    }
}

#[test]
fn no_violations_when_all_under_limit() {
    let counts = vec![("crates/a".into(), 100), ("crates/b".into(), 200)];
    let r = rule("crates/*", 500);
    assert!(find_crate_violations(&counts, &r).is_empty());
}

#[test]
fn one_diagnostic_per_violation() {
    let counts = vec![("crates/a".into(), 600), ("crates/b".into(), 200)];
    let r = rule("crates/*", 500);
    let diags = find_crate_violations(&counts, &r);
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("600"));
    assert_eq!(diags[0].lint, LintId::CrateSize.id());
}

#[test]
fn multiple_violations_each_emit_diagnostic() {
    let counts = vec![
        ("crates/a".into(), 600),
        ("crates/b".into(), 900),
        ("crates/c".into(), 700),
    ];
    let r = rule("crates/*", 500);
    let diags = find_crate_violations(&counts, &r);
    assert_eq!(diags.len(), 3);
    let messages: Vec<&str> = diags.iter().map(|d| d.message.as_str()).collect();
    assert!(messages.iter().any(|m| m.contains("600")));
    assert!(messages.iter().any(|m| m.contains("900")));
    assert!(messages.iter().any(|m| m.contains("700")));
}

#[test]
fn expand_glob_finds_dirs() {
    let tmp = TempDir::new().unwrap();
    let parent = tmp.path().join("crates");
    std::fs::create_dir(&parent).unwrap();
    std::fs::create_dir(parent.join("alpha")).unwrap();
    std::fs::create_dir(parent.join("beta")).unwrap();
    std::fs::write(parent.join("readme.md"), "").unwrap();

    let pattern = format!("{}/*", parent.display());
    let dirs = expand_glob(&pattern);
    assert_eq!(dirs.len(), 2);
}

#[test]
fn expand_glob_empty_parent() {
    let tmp = TempDir::new().unwrap();
    let parent = tmp.path().join("empty");
    std::fs::create_dir(&parent).unwrap();

    let pattern = format!("{}/*", parent.display());
    let dirs = expand_glob(&pattern);
    assert!(dirs.is_empty());
}

#[test]
fn expand_glob_nonexistent_parent() {
    let dirs = expand_glob("/nonexistent/path/*");
    assert!(dirs.is_empty());
}
