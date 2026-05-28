use super::*;
use crate::diagnostic::Diagnostic;

fn compare_version(
    cli_version: &str,
    crate_name: &str,
    lock_packages: &[(String, String)],
) -> Option<Diagnostic> {
    let lock_version = lock_packages
        .iter()
        .find(|(name, _)| name == crate_name)
        .map(|(_, version)| version.as_str())?;

    if cli_version != lock_version {
        Some(
            at_workspace(
                LintId::CliCrateVersion.id(),
                format!(
                    "`{crate_name}` CLI version {cli_version} does not match Cargo.lock {lock_version}"
                ),
            )
            .build(),
        )
    } else {
        None
    }
}

#[test]
fn parse_lock_basic() {
    let content = r#"
[[package]]
name = "serde"
version = "1.0.200"

[[package]]
name = "tokio"
version = "1.37.0"
"#;
    let pkgs = parse_lock_packages(content);
    assert_eq!(pkgs.len(), 2);
    assert_eq!(pkgs[0], ("serde".into(), "1.0.200".into()));
    assert_eq!(pkgs[1], ("tokio".into(), "1.37.0".into()));
}

#[test]
fn parse_lock_empty() {
    let pkgs = parse_lock_packages("");
    assert!(pkgs.is_empty());
}

#[test]
fn parse_lock_no_package_key() {
    let content = r#"
[metadata]
foo = "bar"
"#;
    let pkgs = parse_lock_packages(content);
    assert!(pkgs.is_empty());
}

#[test]
fn parse_lock_skips_incomplete_entries() {
    let content = r#"
[[package]]
name = "incomplete"

[[package]]
name = "ok"
version = "1.0"
"#;
    let pkgs = parse_lock_packages(content);
    assert_eq!(pkgs.len(), 1);
    assert_eq!(pkgs[0].0, "ok");
}

#[test]
fn compare_version_match() {
    let pkgs = vec![("wasm-bindgen".into(), "0.2.90".into())];
    assert!(compare_version("0.2.90", "wasm-bindgen", &pkgs).is_none());
}

#[test]
fn compare_version_mismatch() {
    let pkgs = vec![("wasm-bindgen".into(), "0.2.90".into())];
    let d = compare_version("0.2.89", "wasm-bindgen", &pkgs).unwrap();
    assert_eq!(d.lint, LintId::CliCrateVersion.id());
    assert!(d.message.contains("wasm-bindgen"));
    assert!(d.message.contains("0.2.89"));
    assert!(d.message.contains("0.2.90"));
}

#[test]
fn compare_version_crate_not_in_lock() {
    let pkgs = vec![("serde".into(), "1.0".into())];
    assert!(compare_version("1.0", "missing-crate", &pkgs).is_none());
}

#[test]
fn compare_version_empty_packages() {
    assert!(compare_version("1.0", "any", &[]).is_none());
}

#[test]
fn compare_version_multiple_packages() {
    let pkgs = vec![
        ("alpha".into(), "1.0".into()),
        ("beta".into(), "2.0".into()),
        ("gamma".into(), "3.0".into()),
    ];
    assert!(compare_version("2.0", "beta", &pkgs).is_none());
    assert!(compare_version("999", "beta", &pkgs).is_some());
}
