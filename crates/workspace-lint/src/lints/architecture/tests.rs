use super::*;

fn rule(from: &[&str], deny: &[&str]) -> ArchitectureRule {
    ArchitectureRule {
        name: Some("test-rule".into()),
        from: from.iter().map(|s| s.to_string()).collect(),
        deny: deny.iter().map(|s| s.to_string()).collect(),
        exceptions: Vec::new(),
        severity: ArchSeverity::Warn,
        reason: None,
        suggest: None,
    }
}

#[test]
fn empty_config_yields_no_diagnostics() {
    let cfg = ArchitectureConfig::default();
    assert!(CompiledRule::compile(&rule(&[], &["x"])).is_none());
    assert!(CompiledRule::compile(&rule(&["x"], &[])).is_none());
    let _ = cfg;
}

#[test]
fn deny_pattern_matches_via_glob_form() {
    let r = CompiledRule::compile(&rule(&["apps-*"], &["data-models::internal::**"])).unwrap();
    assert!(r.matches_from("apps-dashboard"));
    assert!(!r.matches_from("ui-shared"));

    let denied = ResolvedPath::new(["data_models", "internal", "User"]);
    let allowed = ResolvedPath::new(["data_models", "api", "User"]);
    assert!(r.denies(&denied));
    assert!(!r.denies(&allowed));
}

#[test]
fn exception_overrides_deny() {
    let mut rl = rule(&["apps-*"], &["sqlx::**"]);
    rl.exceptions = vec!["sqlx::query::Query".into()];
    let r = CompiledRule::compile(&rl).unwrap();
    let denied = ResolvedPath::new(["sqlx", "Pool"]);
    let exception = ResolvedPath::new(["sqlx", "query", "Query"]);
    assert!(r.denies(&denied) && !r.is_exception(&denied));
    assert!(r.denies(&exception) && r.is_exception(&exception));
}
