use predicates::prelude::*;
use std::path::Path;

mod common;
use common::{TestWorkspace, workspace_lint};

fn fixture(name: &str) -> &Path {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    // Leak to get a &'static Path — fine for tests
    Box::leak(p.into_boxed_path())
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
    TestWorkspace::new().config(config_body).write(tmp.path());
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

// --- `check <lint>` CLI overrides (the `from_cli` path, which needs no config
//     file — the rule is built entirely from flags). ---

#[test]
fn check_file_size_cli_override_flags_oversized() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    TestWorkspace::new()
        .loose_file("src/big.rs", "fn a() {}\nfn b() {}\nfn c() {}\n")
        .write(tmp.path());
    workspace_lint()
        .current_dir(tmp.path())
        .args([
            "check",
            "file-size",
            "--glob",
            "**/*.rs",
            "--max-code-lines",
            "2",
        ])
        .assert()
        .stderr(
            predicate::str::contains("exceeds 2 code lines")
                .and(predicate::str::contains("big.rs")),
        );
}

#[test]
fn check_crate_size_cli_override_flags_oversized() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    TestWorkspace::new()
        .resolver("2")
        .lib_member(
            "crates/demo",
            "demo",
            "0.0.1",
            "pub fn a() {}\npub fn b() {}\npub fn c() {}\n",
        )
        .write(tmp.path());
    workspace_lint()
        .current_dir(tmp.path())
        .args([
            "check",
            "crate-size",
            "--glob",
            "crates/*",
            "--max-code-lines",
            "2",
        ])
        .assert()
        .stderr(predicate::str::contains("crate exceeds 2 code lines"));
}

// --- expand: a file mutator, so it only runs under `--fix` (on a clean tree)
//     or via the explicit `expand` subcommand (also clean-tree-gated). A plain
//     run must never rewrite files. ---

const DOC_BEFORE: &str = "head\n<!-- V_START -->\nold\n<!-- V_END -->\ntail\n";

/// A workspace whose config defines an `[[expand.rules]]` table. All lints are
/// `allow`-ed so the plain run is a clean no-op and only expand's side effects
/// (or their absence) are under test. The command is `cargo --version` — present
/// cross-platform and never equal to the placeholder body, so a successful
/// expansion is detectable without depending on its exact output.
fn expand_workspace(tmp: &Path) {
    TestWorkspace::new()
        .config(
            "[lints]\ndefault = \"allow\"\n\n\
             [[expand.rules]]\n\
             command = [\"cargo\", \"--version\"]\n\
             glob = \"DOC.md\"\n\
             marker = \"V\"\n",
        )
        .loose_file("DOC.md", DOC_BEFORE)
        .write(tmp);
}

fn git(dir: &Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("git available")
        .status
        .success();
    assert!(ok, "git {args:?} failed");
}

#[test]
fn plain_run_does_not_expand() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    expand_workspace(tmp.path());
    workspace_lint().current_dir(tmp.path()).assert().success();
    let after = std::fs::read_to_string(tmp.path().join("DOC.md")).unwrap();
    assert_eq!(
        after, DOC_BEFORE,
        "a plain (non-`--fix`) run must not rewrite files"
    );
}

#[test]
fn fix_run_applies_expand() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    expand_workspace(tmp.path());
    // Not a git repo → the clean-tree gate warns and proceeds; `--no-deep`
    // keeps it hermetic (no rust-analyzer).
    workspace_lint()
        .current_dir(tmp.path())
        .args(["--fix", "--no-deep"])
        .assert()
        .success();
    let after = std::fs::read_to_string(tmp.path().join("DOC.md")).unwrap();
    assert!(
        after.contains("cargo ") && !after.contains("old"),
        "`--fix` should apply the configured expand rule; got:\n{after}"
    );
}

#[test]
fn expand_subcommand_is_clean_tree_gated() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    expand_workspace(tmp.path());
    git(tmp.path(), &["init", "-q"]);
    git(tmp.path(), &["config", "user.email", "t@t.test"]);
    git(tmp.path(), &["config", "user.name", "Test"]);
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-q", "-m", "init"]);
    // Dirty a tracked file so the tree is unclean.
    std::fs::write(
        tmp.path().join("DOC.md"),
        "head\n<!-- V_START -->\nedited\n<!-- V_END -->\ntail\n",
    )
    .unwrap();

    let args = [
        "expand",
        "--command",
        "cargo --version",
        "--glob",
        "DOC.md",
        "--marker",
        "V",
    ];
    // Without an override, the dirty tree blocks the mutating subcommand.
    workspace_lint()
        .current_dir(tmp.path())
        .args(args)
        .assert()
        .failure()
        .stderr(predicate::str::contains("clean git working tree"));
    let blocked = std::fs::read_to_string(tmp.path().join("DOC.md")).unwrap();
    assert!(
        blocked.contains("edited"),
        "the gate must run before any write"
    );

    // `--allow-dirty` overrides the gate and the expansion proceeds.
    workspace_lint()
        .current_dir(tmp.path())
        .args(args)
        .arg("--allow-dirty")
        .assert()
        .success();
    let expanded = std::fs::read_to_string(tmp.path().join("DOC.md")).unwrap();
    assert!(
        expanded.contains("cargo ") && !expanded.contains("edited"),
        "with --allow-dirty the subcommand should expand; got:\n{expanded}"
    );
}
