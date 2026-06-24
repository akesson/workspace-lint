use super::*;

fn rule(from: &[&str], deny: &[&str]) -> ArchitectureRule {
    ArchitectureRule {
        name: Some("test-rule".into()),
        from: from.iter().map(|s| s.to_string()).collect(),
        deny: deny.iter().map(|s| s.to_string()).collect(),
        exceptions: Vec::new(),
        severity: None,
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

#[test]
fn denies_via_prefix_when_trailing_method() {
    // A fully-qualified call `data_models::internal::InternalUser::new()` resolves
    // to a 4-segment canonical with a trailing method. The code-reference pass
    // tests every prefix and reports the shortest denied one — the type itself,
    // not the trailing method.
    let r = CompiledRule::compile(&rule(&["apps-*"], &["data-models::internal::**"])).unwrap();
    let canonical = ResolvedPath::new(["data_models", "internal", "InternalUser", "new"]);
    let denied = canonical_prefixes(&canonical)
        .into_iter()
        .find(|p| r.denies(p));
    assert_eq!(
        denied.map(|p| p.display().to_string()).as_deref(),
        Some("data_models::internal::InternalUser"),
        "a trailing-method path must match via its type prefix",
    );
}

#[test]
fn exception_on_prefix_exempts_call_site() {
    // deny `internal::**` with an exception for `internal::PublicToken`. A call
    // `PublicToken::issue()` has a denied prefix, but the `PublicToken` prefix is
    // an exception — so the whole reference is exempt (matching the loop's
    // "any prefix is an exception ⇒ skip", which keeps an exception effective
    // even against a broad `**` rule).
    let mut rl = rule(&["apps-*"], &["data-models::internal::**"]);
    rl.exceptions = vec!["data-models::internal::PublicToken".into()];
    let r = CompiledRule::compile(&rl).unwrap();
    let canonical = ResolvedPath::new(["data_models", "internal", "PublicToken", "issue"]);
    let prefixes = canonical_prefixes(&canonical);
    assert!(
        prefixes.iter().any(|p| r.denies(p)),
        "some prefix matches the deny",
    );
    assert!(
        prefixes.iter().any(|p| r.is_exception(p)),
        "the PublicToken prefix is an exception, exempting the call site",
    );
}
