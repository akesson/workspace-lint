//! `--baseline-write` integration: the build-free half of the duplicate-code
//! baseline ratchet. Generating the baseline never runs the semantic engine
//! (it collects exactly the groups the lint would report via the build-free
//! `collect_baseline`), so these tests need no pinned toolchain. The *read*
//! side — skip / grew / stale through the full engine — is covered by the
//! `tests/cases/duplicate-code/*baseline*` fixtures and the `baseline` unit
//! tests in `wl-lints`.

mod common;

use common::{TestWorkspace, workspace_lint};
use tempfile::TempDir;

/// Two structurally identical fns (names differ) — one clone group.
const CLONE_LIB: &str = "\
pub fn compute(data: &[u32]) -> u32 {
    let mut acc = 0u32;
    for value in data.iter() {
        let scaled = value.wrapping_mul(3);
        acc = acc.wrapping_add(scaled);
    }
    acc.wrapping_sub(7)
}

pub fn tally(input: &[u32]) -> u32 {
    let mut sum = 0u32;
    for item in input.iter() {
        let weighted = item.wrapping_mul(5);
        sum = sum.wrapping_add(weighted);
    }
    sum.wrapping_sub(9)
}
";

const CONFIG_WITH_BASELINE: &str = "\
[lints]
default = \"allow\"
duplicate-code = \"deny\"

[duplicate-code]
min-lines = 4
min-tokens = 20
min-distinct-anchors = 0
min-non-repeating-ratio = 0.0
classify = false
baseline = \"dup.toml\"
";

fn workspace_with(config: &str) -> TestWorkspace {
    TestWorkspace::new()
        .resolver("2")
        .lib_member("crates/demo", "demo", "0.1.0", CLONE_LIB)
        .config(config)
}

#[test]
fn baseline_write_produces_a_parseable_file() {
    let dir = TempDir::new().unwrap();
    workspace_with(CONFIG_WITH_BASELINE).write(dir.path());

    workspace_lint()
        .current_dir(dir.path())
        .arg("--baseline-write")
        .assert()
        .success()
        .stderr(predicates::str::contains(
            "wrote 1 clone group(s) to dup.toml",
        ));

    let baseline = std::fs::read_to_string(dir.path().join("dup.toml")).unwrap();
    assert!(baseline.contains("[[group]]"), "wrote a group entry");
    assert!(baseline.contains("fingerprint = \""), "wrote a fingerprint");
    assert!(
        baseline.contains("instances = 2"),
        "recorded both instances"
    );
    // The generated file must round-trip: a second lint run finds nothing new.
    // (Full read-path coverage is in the cases fixtures.)
}

#[test]
fn baseline_write_is_deterministic_across_directories() {
    // The fingerprint is content-addressed and the anchor path is
    // workspace-relative, so the file must be byte-identical regardless of
    // where the workspace lives — the property a checked-in baseline needs.
    let a = TempDir::new().unwrap();
    let b = TempDir::new().unwrap();
    workspace_with(CONFIG_WITH_BASELINE).write(a.path());
    workspace_with(CONFIG_WITH_BASELINE).write(b.path());

    for dir in [a.path(), b.path()] {
        workspace_lint()
            .current_dir(dir)
            .arg("--baseline-write")
            .assert()
            .success();
    }

    let left = std::fs::read(a.path().join("dup.toml")).unwrap();
    let right = std::fs::read(b.path().join("dup.toml")).unwrap();
    assert_eq!(
        left, right,
        "baseline must be byte-identical across locations"
    );
}

#[test]
fn baseline_write_without_table_fails() {
    let dir = TempDir::new().unwrap();
    workspace_with("[lints]\ndefault = \"allow\"\n").write(dir.path());

    workspace_lint()
        .current_dir(dir.path())
        .arg("--baseline-write")
        .assert()
        .code(2)
        .stderr(predicates::str::contains("[duplicate-code] table"));
}

#[test]
fn baseline_write_without_key_fails() {
    let config = "\
[lints]
default = \"allow\"
duplicate-code = \"deny\"

[duplicate-code]
min-lines = 4
";
    let dir = TempDir::new().unwrap();
    workspace_with(config).write(dir.path());

    workspace_lint()
        .current_dir(dir.path())
        .arg("--baseline-write")
        .assert()
        .code(2)
        .stderr(predicates::str::contains("baseline = "));
}

#[test]
fn check_subcommand_rejects_baseline_write() {
    // `--baseline-write` is default-run only: the baseline must be generated
    // under the repo config CI lints with, not an ad-hoc `check` flag set.
    let dir = TempDir::new().unwrap();
    workspace_with(CONFIG_WITH_BASELINE).write(dir.path());

    workspace_lint()
        .current_dir(dir.path())
        .args(["--baseline-write", "check", "duplicate-code"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("only works on the default run"));
}
