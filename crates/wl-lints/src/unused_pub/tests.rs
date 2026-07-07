use super::*;
use crate::unused_pub::ir;
use wl_lint_api::config::GlobPattern;

#[test]
fn glob_set_returns_none_for_empty() {
    assert!(ir::build_glob_set(&[]).is_none());
}

#[test]
fn glob_set_matches_canonical_path_patterns() {
    let set = ir::build_glob_set(&[GlobPattern::new("*Error").unwrap()]).unwrap();
    assert!(set.is_match("MyError"));
    assert!(!set.is_match("Thing"));
}

#[test]
fn kind_filter_ir_vocabulary_is_total() {
    let all = [
        (KindFilter::Function, "fn"),
        (KindFilter::Struct, "struct"),
        (KindFilter::Enum, "enum"),
        (KindFilter::Union, "union"),
        (KindFilter::Trait, "trait"),
        (KindFilter::Type, "type"),
        (KindFilter::Const, "const"),
        (KindFilter::Static, "static"),
        (KindFilter::Module, "mod"),
        (KindFilter::Macro, "macro"),
    ];
    for (filter, expected) in all {
        assert_eq!(filter.to_ir_kind(), expected);
    }
}
