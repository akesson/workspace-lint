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

// architecture tests live in tests/cases.rs (four-kind taxonomy)

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

// --- config fail-fast paths (parse errors abort the run with exit 1) ---

/// Write a minimal workspace whose standalone config is `config_body`, run the
/// binary there, and return the assert handle.
fn run_with_config(config_body: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("create tempdir");
    std::fs::write(tmp.path().join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
    std::fs::write(tmp.path().join(".workspace-lint.toml"), config_body).unwrap();
    tmp
}

#[test]
fn malformed_toml_config_aborts() {
    let tmp = run_with_config("this is = = not toml\n");
    workspace_lint()
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("failed to parse config"));
}

#[test]
fn invalid_lint_level_aborts_with_variants() {
    let tmp = run_with_config("[lints]\ncentralized-deps = \"lou\"\n");
    workspace_lint()
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("failed to parse config").and(predicate::str::contains(
                "expected one of `allow`, `warn`, `deny`",
            )),
        );
}

#[test]
fn invalid_glob_in_config_aborts() {
    let tmp = run_with_config("[[file-size.rules]]\nglob = \"[unclosed\"\nmax-code-lines = 10\n");
    workspace_lint()
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid glob"));
}
