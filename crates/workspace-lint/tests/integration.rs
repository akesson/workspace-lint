use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use std::path::Path;

fn fixture(name: &str) -> &Path {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    // Leak to get a &'static Path — fine for tests
    Box::leak(p.into_boxed_path())
}

fn workspace_lint() -> assert_cmd::Command {
    cargo_bin_cmd!("workspace-lint")
}

// --- centralized-deps ---

#[test]
fn centralized_deps_clean_passes() {
    workspace_lint()
        .current_dir(fixture("centralized_deps_clean"))
        .args(["check", "centralized-deps"])
        .assert()
        .success()
        .stderr(predicate::str::contains("all passed"));
}

#[test]
fn centralized_deps_violation_fails() {
    workspace_lint()
        .current_dir(fixture("centralized_deps_violation"))
        .args(["check", "centralized-deps"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("serde").and(predicate::str::contains("workspace = true")),
        );
}

// --- unused-deps ---

#[test]
fn unused_deps_clean_passes() {
    workspace_lint()
        .current_dir(fixture("unused_deps_clean"))
        .args(["check", "unused-deps"])
        .assert()
        .success()
        .stderr(predicate::str::contains("all passed"));
}

#[test]
fn unused_deps_violation_fails() {
    workspace_lint()
        .current_dir(fixture("unused_deps_violation"))
        .args(["check", "unused-deps"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("rand"));
}

// --- config loading ---

#[test]
fn config_standalone_loads() {
    workspace_lint()
        .current_dir(fixture("config_standalone"))
        .assert()
        .success()
        .stderr(predicate::str::contains("all passed"));
}

#[test]
fn config_cargo_metadata_loads() {
    workspace_lint()
        .current_dir(fixture("config_cargo_metadata"))
        .assert()
        .success()
        .stderr(predicate::str::contains("all passed"));
}

#[test]
fn config_both_sources_errors() {
    workspace_lint()
        .current_dir(fixture("config_both"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("use only one"));
}

#[test]
fn no_config_errors() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    workspace_lint()
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("no configuration found"));
}
