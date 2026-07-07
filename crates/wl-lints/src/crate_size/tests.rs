// These tests call the production `find_crate_violations` directly; the
// member-discovery + tokei walk in `check` is exercised end-to-end by
// tests/cases/crate-size.
use super::*;

fn rule(glob: &str, max: usize) -> CrateSizeRule {
    CrateSizeRule {
        glob: GlobPattern::new(glob).unwrap(),
        max_code_lines: max,
        include: None,
    }
}

#[test]
fn no_violations_when_all_under_limit() {
    let counts = vec![("crates/a".into(), 100), ("crates/b".into(), 200)];
    let r = rule("crates/*", 500);
    assert!(find_crate_violations(&r, &counts).is_empty());
}

#[test]
fn one_diagnostic_per_violation() {
    let counts = vec![("crates/a".into(), 600), ("crates/b".into(), 200)];
    let r = rule("crates/*", 500);
    let diags = find_crate_violations(&r, &counts);
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
    let diags = find_crate_violations(&r, &counts);
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
