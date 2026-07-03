use super::*;

/// Segment-vec shorthand for the canonical paths the rule machinery judges.
fn path(segments: &[&str]) -> Vec<String> {
    segments.iter().map(|s| s.to_string()).collect()
}

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

    let denied = path(&["data_models", "internal", "User"]);
    let allowed = path(&["data_models", "api", "User"]);
    assert!(r.denies(&denied));
    assert!(!r.denies(&allowed));
}

#[test]
fn exception_overrides_deny() {
    let mut rl = rule(&["apps-*"], &["sqlx::**"]);
    rl.exceptions = vec!["sqlx::query::Query".into()];
    let r = CompiledRule::compile(&rl).unwrap();
    let denied = path(&["sqlx", "Pool"]);
    let exception = path(&["sqlx", "query", "Query"]);
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
    let canonical = path(&["data_models", "internal", "InternalUser", "new"]);
    let denied = canonical_prefixes(&canonical)
        .into_iter()
        .find(|p| r.denies(p));
    assert_eq!(
        denied.map(|p| p.join("::")).as_deref(),
        Some("data_models::internal::InternalUser"),
        "a trailing-method path must match via its type prefix",
    );
}

#[test]
fn exception_at_denied_prefix_exempts_call_site() {
    // deny `internal::**` with an exception for `internal::PublicToken`. A call
    // `PublicToken::issue()` resolves to `internal::PublicToken::issue`: the
    // denied prefix is `...::PublicToken`, and the exception sits *at* that
    // prefix — so the at-or-below slice the code-ref pass checks sees it and the
    // whole reference is exempt, keeping an exception effective even under a
    // broad `**` rule.
    let mut rl = rule(&["apps-*"], &["data-models::internal::**"]);
    rl.exceptions = vec!["data-models::internal::PublicToken".into()];
    let r = CompiledRule::compile(&rl).unwrap();
    let canonical = path(&["data_models", "internal", "PublicToken", "issue"]);
    let prefixes = canonical_prefixes(&canonical);
    let denied_idx = prefixes
        .iter()
        .position(|p| r.denies(p))
        .expect("a prefix matches the deny");
    assert!(
        prefixes[denied_idx..].iter().any(|p| r.is_exception(p)),
        "the PublicToken exception is at the denied prefix, exempting the call site",
    );
}

#[test]
fn exception_above_denied_prefix_does_not_exempt() {
    // deny `internal::**`, but the exception is the bare `internal` MODULE — a
    // strict ancestor of the denied item. A call `Secret::new()` resolves to
    // `internal::Secret::new`: the denied prefix is `...::Secret`, and the
    // exception lies ABOVE it. The code-ref pass exempts only at or below the
    // denied prefix, so this reference still fires — matching the `use` pass,
    // which checks the imported item itself, not an ancestor module.
    let mut rl = rule(&["apps-*"], &["data-models::internal::**"]);
    rl.exceptions = vec!["data-models::internal".into()];
    let r = CompiledRule::compile(&rl).unwrap();
    let canonical = path(&["data_models", "internal", "Secret", "new"]);
    let prefixes = canonical_prefixes(&canonical);
    let denied_idx = prefixes
        .iter()
        .position(|p| r.denies(p))
        .expect("a prefix matches the deny");
    assert_eq!(
        prefixes[denied_idx].join("::"),
        "data_models::internal::Secret",
        "shortest denied prefix is the type, not the ancestor module",
    );
    assert!(
        !prefixes[denied_idx..].iter().any(|p| r.is_exception(p)),
        "an ancestor-module exception must not exempt the denied item",
    );
    // The over-broad old predicate (any prefix) WOULD have wrongly exempted it.
    assert!(
        prefixes.iter().any(|p| r.is_exception(p)),
        "the ancestor module is indeed an exception prefix — the old false-exempt",
    );
}
