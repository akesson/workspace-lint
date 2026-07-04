//! Unit tests for the pure `declared_never_gated` decision. The full
//! resolver-fed pipeline (`check`) is covered end-to-end by
//! `tests/cases/feature_drift/`; here we pin the leaf-vs-umbrella /
//! `default` / empty-name logic in isolation, where it's cheap to enumerate.

use std::collections::BTreeSet;

use super::should_flag_declared;

/// Helper: build a `BTreeSet<&str>` of "gated in source" feature names.
fn used<'a>(names: &[&'a str]) -> BTreeSet<&'a str> {
    names.iter().copied().collect()
}

#[test]
fn leaf_feature_never_gated_is_flagged() {
    // Empty activation list (a leaf feature) that no `cfg(feature = ...)`
    // references → the real drift we want to catch.
    assert!(should_flag_declared("extra", Some(&[]), &used(&[])));
    // Also flagged when the feature isn't present in the values map at all.
    assert!(should_flag_declared("extra", None, &used(&[])));
}

#[test]
fn leaf_feature_that_is_gated_is_not_flagged() {
    assert!(!should_flag_declared("extra", Some(&[]), &used(&["extra"])));
}

#[test]
fn default_is_never_flagged() {
    // cargo handles `default` specially; it legitimately need not be gated.
    assert!(!should_flag_declared("default", Some(&[]), &used(&[])));
    assert!(!should_flag_declared(
        "default",
        Some(&["greet".to_string()]),
        &used(&[]),
    ));
}

#[test]
fn empty_feature_name_is_never_flagged() {
    assert!(!should_flag_declared("", Some(&[]), &used(&[])));
}

#[test]
fn umbrella_feature_forwarding_to_others_is_not_flagged() {
    // `full = ["greet", "shout"]` forwards to other features; it need not
    // appear in a `cfg(feature = "full")` gate.
    let activation = vec!["greet".to_string(), "shout".to_string()];
    assert!(!should_flag_declared("full", Some(&activation), &used(&[])));
}

#[test]
fn dep_activation_feature_is_not_flagged() {
    // `tls = ["dep:openssl"]` plumbs an optional dependency; never gated.
    let activation = vec!["dep:openssl".to_string()];
    assert!(!should_flag_declared("tls", Some(&activation), &used(&[])));
}

#[test]
fn weak_and_slash_dep_activation_is_not_flagged() {
    // `foo/bar` and `foo?/bar` forward to a dependency's feature — non-empty
    // activation list, so never expected as a direct `cfg(feature = ...)` gate.
    let slash = vec!["optdep/featx".to_string()];
    assert!(!should_flag_declared("strong", Some(&slash), &used(&[])));
    let weak = vec!["optdep?/featx".to_string()];
    assert!(!should_flag_declared("weak", Some(&weak), &used(&[])));
}
