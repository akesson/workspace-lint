use super::*;

#[test]
fn kind_filter_parses_aliases() {
    let filter = parse_kind_filter(&["fn".into(), "function".into(), "type".into()]).unwrap();
    assert!(filter.contains(&ItemKind::Fn));
    assert!(filter.contains(&ItemKind::TypeAlias));
}

#[test]
fn kind_filter_ignores_unknown_kinds() {
    let filter = parse_kind_filter(&["banana".into(), "fn".into()]).unwrap();
    assert_eq!(filter.len(), 1);
    assert!(filter.contains(&ItemKind::Fn));
}

#[test]
fn kind_filter_empty_returns_none() {
    assert!(parse_kind_filter(&[]).is_none());
}

#[test]
fn glob_set_returns_none_for_empty() {
    assert!(build_glob_set(&[], "test").is_none());
}
