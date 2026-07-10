use predicates::prelude::*;
use std::path::Path;

mod common;
use common::{TestWorkspace, copy_tree, workspace_lint};

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

/// `--fast-only` must *skip* the semantic lints, not run them without their
/// model — regression guard: this panicked ("unused-deps requires the
/// SemanticModel") on any semantic-lint-enabled workspace from the first
/// port until the corpus fast-tier smoke caught it.
#[test]
fn fast_only_skips_semantic_lints() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    copy_tree(fixture("unused_deps_clean"), tmp.path()).expect("copy fixture");
    std::fs::write(
        tmp.path().join(".workspace-lint.toml"),
        "[unused-deps]\n[unused-pub]\n",
    )
    .expect("write config");
    workspace_lint()
        .current_dir(tmp.path())
        .arg("--fast-only")
        .assert()
        .success();
}

/// An `expect` for a semantic lint must not report stale under `--fast-only`:
/// the lint never ran, so "unmatched" carries no staleness signal — regression
/// guard for the 14 false stale-expects the first real-world `--fast-only`
/// run emitted (one per committed `expect(unused-deps)`).
#[test]
fn fast_only_does_not_stale_semantic_expects() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    copy_tree(fixture("unused_deps_clean"), tmp.path()).expect("copy fixture");
    std::fs::write(
        tmp.path().join(".workspace-lint.toml"),
        "[unused-deps]\n[unused-pub]\n",
    )
    .expect("write config");
    let manifest = tmp.path().join("crates/alpha/Cargo.toml");
    let mut text = std::fs::read_to_string(&manifest).expect("read member manifest");
    text.push_str("\n# workspace-lint: expect(unused-deps)\n");
    std::fs::write(&manifest, text).expect("append expect directive");
    workspace_lint()
        .current_dir(tmp.path())
        .arg("--fast-only")
        .assert()
        .success()
        .stderr(predicate::str::contains("stale").not());
}

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

/// A run from INSIDE a member directory re-roots to the configured workspace
/// root (with a notice) instead of dead-ending on "no configuration found" —
/// cargo itself walks up the same way, so the old literal-cwd-only load was
/// the surprising one (2026-07-10 validation, Issue 10).
#[test]
fn member_dir_run_reroots_to_configured_root() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    copy_tree(fixture("unused_deps_clean"), tmp.path()).expect("copy fixture");
    std::fs::write(
        tmp.path().join(".workspace-lint.toml"),
        "[unused-deps]\n[lints]\ndefault = \"allow\"\nunused-deps = \"warn\"\n",
    )
    .expect("write config");
    workspace_lint()
        .current_dir(tmp.path().join("crates/alpha"))
        .arg("--fast-only")
        .assert()
        .success()
        .stderr(predicate::str::contains("running at workspace root"))
        .stderr(predicate::str::contains("all passed"));
}

/// With no config anywhere but a `[workspace]` root above, the error names
/// that root — the generic "run `workspace-lint init`" hint pointed at the
/// member dir, where `init` refuses to scaffold.
#[test]
fn member_dir_run_names_unconfigured_workspace_root() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    copy_tree(fixture("unused_deps_clean"), tmp.path()).expect("copy fixture");
    workspace_lint()
        .current_dir(tmp.path().join("crates/alpha"))
        .arg("--fast-only")
        .assert()
        .failure()
        .stderr(predicate::str::contains("the workspace root is"))
        .stderr(predicate::str::contains("run `workspace-lint init` there"));
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
    let ok = common::git(dir)
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
    // Not a git repo → the clean-tree gate warns and proceeds.
    workspace_lint()
        .current_dir(tmp.path())
        .args(["--fix"])
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

// --- init: scaffold a default config, but only at a workspace root ---

/// A one-member virtual workspace with NO `.workspace-lint.toml` — the starting
/// state `init` expects. Given a member so the virtual manifest isn't memberless
/// (cargo errors on a memberless virtual root, which `init` would misreport).
fn init_demo_workspace(tmp: &Path) {
    TestWorkspace::new()
        .lib_member("crates/demo", "demo", "0.0.1", "pub fn a() {}\n")
        .write(tmp);
}

#[test]
fn init_creates_config_at_workspace_root() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    init_demo_workspace(tmp.path());
    workspace_lint()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .success()
        .stderr(predicate::str::contains("created .workspace-lint.toml"));
    let written = std::fs::read_to_string(tmp.path().join(".workspace-lint.toml"))
        .expect("init wrote the config");
    assert!(
        written.contains("centralized-deps = \"deny\""),
        "the scaffolded config should escalate centralized-deps; got:\n{written}"
    );
}

/// The scaffolded config must be immediately usable: a plain run parses and
/// audits it cleanly. `--fast-only` skips the semantic tier so this needs no
/// nightly toolchain.
#[test]
fn init_output_is_a_usable_config() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    init_demo_workspace(tmp.path());
    workspace_lint()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .success();
    workspace_lint()
        .current_dir(tmp.path())
        .arg("--fast-only")
        .assert()
        .success()
        .stderr(predicate::str::contains("all passed"));
}

#[test]
fn init_refuses_to_clobber_without_force() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    init_demo_workspace(tmp.path());
    workspace_lint()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .success();
    // Second run without --force is refused, leaving the file intact.
    workspace_lint()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));
    // --force overwrites.
    workspace_lint()
        .current_dir(tmp.path())
        .args(["init", "--force"])
        .assert()
        .success()
        .stderr(predicate::str::contains("created .workspace-lint.toml"));
}

#[test]
fn init_refuses_inside_a_member() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    init_demo_workspace(tmp.path());
    workspace_lint()
        .current_dir(tmp.path().join("crates/demo"))
        .arg("init")
        .assert()
        .failure()
        .stderr(predicate::str::contains("must run at the workspace root"));
    assert!(
        !tmp.path().join("crates/demo/.workspace-lint.toml").exists(),
        "init must not write a config inside a member"
    );
}

#[test]
fn init_refuses_a_lone_package() {
    // A single `[package]` with no `[workspace]` table — cargo's *implicit*
    // workspace, which `init` must reject. Hand-written (TestWorkspace always
    // emits a `[workspace]` root).
    let tmp = tempfile::tempdir().expect("create tempdir");
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"solo\"\nversion = \"0.0.1\"\nedition = \"2024\"\n\n[lib]\npath = \"src/lib.rs\"\n",
    )
    .unwrap();
    std::fs::write(tmp.path().join("src/lib.rs"), "pub fn a() {}\n").unwrap();
    workspace_lint()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .failure()
        .stderr(predicate::str::contains("no [workspace] table"));
    assert!(
        !tmp.path().join(".workspace-lint.toml").exists(),
        "init must not write a config for a lone package"
    );
}

#[test]
fn init_refuses_outside_a_cargo_project() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    workspace_lint()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .failure()
        .stderr(predicate::str::contains("no loadable Cargo.toml"));
}

// --- GIT_DIR leak hardening ---
//
// Git exports an absolute `GIT_DIR` to hook processes when the repository is
// discovered via a `.git` file (linked worktrees, submodules), and child
// processes inherit it. The binary's git spawns must scrub it, or they operate
// on the invoker's repository with the lint target as its work tree: the
// 2026-07 incident had the pre-push hook's GIT_DIR reach the suite's fixture
// git commands, committing fixture trees onto the developer's real branch and
// flipping `core.bare` in the shared config. These tests leak a GIT_DIR
// pointing at a "victim" repo into the binary and assert repo discovery stays
// cwd-based and the victim is untouched.

/// A committed repo standing in for the developer repository a leaked
/// `GIT_DIR` points at. Its `tracked.txt` deliberately doesn't exist in the
/// lint-target workspaces below.
fn victim_repo() -> tempfile::TempDir {
    let victim = tempfile::tempdir().expect("create tempdir");
    std::fs::write(victim.path().join("tracked.txt"), "v1\n").unwrap();
    git(victim.path(), &["init", "-q"]);
    git(victim.path(), &["config", "user.email", "t@t.test"]);
    git(victim.path(), &["config", "user.name", "Test"]);
    git(victim.path(), &["add", "-A"]);
    git(victim.path(), &["commit", "-q", "-m", "init"]);
    victim
}

/// Captured, trimmed stdout of a git command in `dir` (victim-state probes).
fn git_stdout(dir: &Path, args: &[&str]) -> String {
    let out = common::git(dir).args(args).output().expect("git available");
    assert!(out.status.success(), "git {args:?} failed");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn leaked_git_dir_does_not_reach_git_writes() {
    let victim = victim_repo();
    let head_before = git_stdout(victim.path(), &["rev-parse", "HEAD"]);

    // The expand target is a repo of its own, so the *scrubbed* `git add` has
    // somewhere to stage; in a non-repo it would fail and `expand` exits 2.
    let tmp = tempfile::tempdir().expect("create tempdir");
    std::fs::write(tmp.path().join("DOC.md"), DOC_BEFORE).unwrap();
    git(tmp.path(), &["init", "-q"]);

    // `--allow-dirty` is load-bearing. Both expand entry points run the
    // clean-tree gate first, and unscrubbed that gate reads the *victim* as
    // dirty and exits 2 — the `git add` below would never run, and this test
    // would silently degrade into a copy of `leaked_git_dir_does_not_block_fix`.
    // Skipping the gate is what isolates the write path.
    workspace_lint()
        .current_dir(tmp.path())
        .env("GIT_DIR", victim.path().join(".git"))
        .args([
            "--allow-dirty",
            "expand",
            "--command",
            "cargo --version",
            "--glob",
            "DOC.md",
            "--marker",
            "V",
            "--auto-stage",
        ])
        .assert()
        .success();

    // Unscrubbed, `git add` inherits GIT_DIR and takes the cwd as its work
    // tree, staging the tempdir's DOC.md into the victim's index.
    assert_eq!(
        git_stdout(victim.path(), &["diff", "--cached", "--name-only"]),
        "",
        "leaked GIT_DIR must not let `expand --auto-stage` write the victim's index"
    );
    // The other two damage modes of the incident.
    assert_eq!(
        git_stdout(victim.path(), &["rev-parse", "HEAD"]),
        head_before,
        "leaked GIT_DIR must not let the run move the victim's HEAD"
    );
    assert_eq!(
        git_stdout(victim.path(), &["config", "core.bare"]),
        "false",
        "leaked GIT_DIR must not flip the victim's core.bare"
    );
}

#[test]
fn leaked_git_dir_does_not_block_fix() {
    let victim = victim_repo();

    let tmp = tempfile::tempdir().expect("create tempdir");
    expand_workspace(tmp.path());
    // Unscrubbed, the clean-tree gate would read the victim repo as Dirty
    // (its tracked files are "missing" from the tempdir work tree) and exit 2
    // without applying anything. The tempdir is not a repo, so the gate must
    // take the warn-and-proceed path instead.
    workspace_lint()
        .current_dir(tmp.path())
        .env("GIT_DIR", victim.path().join(".git"))
        .args(["--fix"])
        .assert()
        .success()
        .stderr(predicate::str::contains("not a git repository"));
}

// --- engine failure: the fast tier's findings must survive ---

/// An engine failure (here: a member that doesn't compile) must not swallow
/// the build-free tier's findings. They render through the normal pipeline
/// FIRST, then the engine error prints with the did-not-run trailer and the
/// run exits 2 — regression guard for the campaign dead end where one broken
/// member hid every fast finding behind the extraction error.
#[test]
fn engine_failure_still_renders_fast_findings() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    TestWorkspace::new()
        .lib_member(
            "crates/broken",
            "wl-int-engine-failure",
            "0.0.1",
            "pub fn a() {}\npub fn b() {}\ncompile_error!(\"deliberate failure\");\n",
        )
        .config("[[file-size.rules]]\nglob = \"**/*.rs\"\nmax-code-lines = 1\n\n[unused-pub]\n")
        .write(tmp.path());
    workspace_lint()
        .current_dir(tmp.path())
        .assert()
        .code(2)
        .stderr(
            predicate::str::contains("file-size")
                .and(predicate::str::contains("semantic lints did not run"))
                .and(predicate::function(|s: &str| {
                    // The fast finding renders BEFORE the engine error.
                    match (s.find("file-size"), s.find("semantic lints did not run")) {
                        (Some(finding), Some(trailer)) => finding < trailer,
                        _ => false,
                    }
                })),
        );
}
