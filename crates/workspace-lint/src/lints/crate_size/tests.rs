use super::*;
use crate::diagnostic::Diagnostic;
use crate::diagnostic::builder::at_crate;

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

// `expand_glob` was removed when this lint switched to iterating
// `workspace.members()` for its discovery walk; integration coverage now
// lives in `tests/cases/crate-size/` (when those fixtures land) instead
// of synthesizing filesystem trees in unit tests.
