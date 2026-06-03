//! End-to-end coverage for the `cli-crate-version` lint's subprocess path.
//!
//! The pure version-extraction / lockfile-comparison helpers are unit-tested in
//! `src/lints/cli_crate_version/tests.rs`; this harness spawns the real binary
//! so the `Command` invocation, stdout capture, and — most importantly — the
//! "a broken rule must not abort the whole run" contract are exercised for real.

use std::path::Path;

mod common;
use common::{TestWorkspace, workspace_lint};

/// Write a minimal workspace whose lockfile pins `mytool` to `lock_version`,
/// plus a `.workspace-lint.toml` carrying one cli-crate-version `rule`.
fn write_workspace(dir: &Path, lock_version: &str, rule: &str) {
    write_workspace_rules(dir, lock_version, &[rule]);
}

/// Like [`write_workspace`] but emits one `[[cli-crate-version.rules]]` block
/// per entry in `rules`, so a test can exercise multiple rules in one run.
fn write_workspace_rules(dir: &Path, lock_version: &str, rules: &[&str]) {
    let mut cfg = String::from("[lints]\ndefault = \"allow\"\ncli-crate-version = \"deny\"\n");
    for rule in rules {
        cfg.push_str("\n[[cli-crate-version.rules]]\n");
        cfg.push_str(rule);
        cfg.push('\n');
    }
    TestWorkspace::new()
        .lib_member("crates/mytool", "mytool", lock_version, "pub fn f() {}\n")
        .lock(format!(
            "version = 3\n\n[[package]]\nname = \"mytool\"\nversion = \"{lock_version}\"\n"
        ))
        .config(cfg)
        .write(dir);
}

/// A missing binary must surface as a rendered diagnostic and let the run
/// finish — not abort the whole process the way the old `exit(1)` did.
#[test]
fn missing_binary_is_a_diagnostic_not_an_abort() {
    let tmp = tempfile::tempdir().unwrap();
    write_workspace(
        tmp.path(),
        "1.2.3",
        "command = [\"workspace-lint-no-such-binary-zzz\", \"--version\"]\npattern = \"(\\\\d+\\\\.\\\\d+\\\\.\\\\d+)\"\ncrate = \"mytool\"",
    );

    let output = workspace_lint().current_dir(tmp.path()).output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    // The new rendered-diagnostic path (help + note), not the old `eprintln!`
    // + `process::exit(1)`.
    assert!(stderr.contains("failed to run"), "stderr: {stderr}");
    assert!(
        stderr.contains("ensure the tool is installed"),
        "expected the rendered help line; stderr: {stderr}"
    );
}

#[cfg(unix)]
fn write_fake_tool(dir: &Path, printed_version: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("fake-tool.sh");
    std::fs::write(
        &path,
        format!("#!/bin/sh\necho 'mytool {printed_version}'\n"),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[cfg(unix)]
#[test]
fn matching_version_passes() {
    let tmp = tempfile::tempdir().unwrap();
    let tool = write_fake_tool(tmp.path(), "1.2.3");
    write_workspace(
        tmp.path(),
        "1.2.3",
        &format!(
            "command = [{:?}, \"--version\"]\npattern = \"mytool (\\\\d+\\\\.\\\\d+\\\\.\\\\d+)\"\ncrate = \"mytool\"",
            tool.to_string_lossy()
        ),
    );
    workspace_lint().current_dir(tmp.path()).assert().success();
}

#[cfg(unix)]
#[test]
fn mismatched_version_fails_with_finding() {
    let tmp = tempfile::tempdir().unwrap();
    let tool = write_fake_tool(tmp.path(), "9.9.9");
    write_workspace(
        tmp.path(),
        "1.2.3",
        &format!(
            "command = [{:?}, \"--version\"]\npattern = \"mytool (\\\\d+\\\\.\\\\d+\\\\.\\\\d+)\"\ncrate = \"mytool\"",
            tool.to_string_lossy()
        ),
    );
    let output = workspace_lint().current_dir(tmp.path()).output().unwrap();
    assert!(
        !output.status.success(),
        "deny-level mismatch should exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("CLI version 9.9.9 does not match Cargo.lock 1.2.3"),
        "stderr: {stderr}"
    );
}

/// The core contract behind the per-rule error handling: one broken rule must
/// NOT suppress the others. A rule with a missing binary (a setup error) and a
/// rule with a real mismatch run together — both diagnostics must appear.
#[cfg(unix)]
#[test]
fn one_broken_rule_does_not_suppress_other_rules() {
    let tmp = tempfile::tempdir().unwrap();
    let tool = write_fake_tool(tmp.path(), "9.9.9");
    let broken = "command = [\"workspace-lint-no-such-binary-zzz\", \"--version\"]\npattern = \"(\\\\d+\\\\.\\\\d+\\\\.\\\\d+)\"\ncrate = \"mytool\"";
    let mismatch = format!(
        "command = [{:?}, \"--version\"]\npattern = \"mytool (\\\\d+\\\\.\\\\d+\\\\.\\\\d+)\"\ncrate = \"mytool\"",
        tool.to_string_lossy()
    );
    write_workspace_rules(tmp.path(), "1.2.3", &[broken, &mismatch]);

    let output = workspace_lint().current_dir(tmp.path()).output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    // The broken rule's setup error AND the second rule's mismatch finding both
    // surface — proving the first rule's failure didn't abort evaluation.
    assert!(
        stderr.contains("failed to run"),
        "broken rule's error missing; stderr: {stderr}"
    );
    assert!(
        stderr.contains("CLI version 9.9.9 does not match Cargo.lock 1.2.3"),
        "second rule did not run after the broken one; stderr: {stderr}"
    );
}
