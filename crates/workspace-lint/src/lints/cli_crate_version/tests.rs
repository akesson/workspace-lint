use super::*;

// --- parse_lock_packages (now returns Result) ---

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
    let pkgs = parse_lock_packages(content).unwrap();
    assert_eq!(pkgs.len(), 2);
    assert_eq!(pkgs[0], ("serde".into(), "1.0.200".into()));
    assert_eq!(pkgs[1], ("tokio".into(), "1.37.0".into()));
}

#[test]
fn parse_lock_empty() {
    assert!(parse_lock_packages("").unwrap().is_empty());
}

#[test]
fn parse_lock_no_package_key() {
    let content = "[metadata]\nfoo = \"bar\"\n";
    assert!(parse_lock_packages(content).unwrap().is_empty());
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
    let pkgs = parse_lock_packages(content).unwrap();
    assert_eq!(pkgs.len(), 1);
    assert_eq!(pkgs[0].0, "ok");
}

#[test]
fn parse_lock_invalid_toml_is_err() {
    let err = parse_lock_packages("this is = = not toml").unwrap_err();
    assert!(err.contains("Cargo.lock"), "{err}");
}

// --- extract_version (the production regex+ANSI path) ---

#[test]
fn extract_version_captures_group_one() {
    let re = Regex::new(r"version (\d+\.\d+\.\d+)").unwrap();
    let out = b"mytool version 0.2.90\n";
    assert_eq!(extract_version(out, &re).as_deref(), Some("0.2.90"));
}

#[test]
fn extract_version_strips_ansi_and_trims() {
    let re = Regex::new(r"(\d+\.\d+\.\d+)").unwrap();
    // ANSI-colored output with a trailing newline — must still extract clean.
    let out = b"\x1b[32m1.2.3\x1b[0m\n";
    assert_eq!(extract_version(out, &re).as_deref(), Some("1.2.3"));
}

#[test]
fn extract_version_no_match_is_none() {
    let re = Regex::new(r"v(\d+)").unwrap();
    assert!(extract_version(b"no version here", &re).is_none());
}

// --- find_lock_version ---

#[test]
fn find_lock_version_present_and_absent() {
    let pkgs = vec![
        ("alpha".into(), "1.0".into()),
        ("beta".into(), "2.0".into()),
    ];
    assert_eq!(find_lock_version(&pkgs, "beta"), Some("2.0"));
    assert_eq!(find_lock_version(&pkgs, "missing"), None);
    assert_eq!(find_lock_version(&[], "any"), None);
}

// --- check_rule: failures are diagnostics, never process exits ---

fn rule(command: &[&str], pattern: &str, crate_name: &str) -> CliCrateVersionRule {
    CliCrateVersionRule {
        command: command.iter().map(|s| s.to_string()).collect(),
        pattern: pattern.to_string(),
        crate_name: crate_name.to_string(),
    }
}

#[test]
fn check_rule_empty_command_is_error_not_panic() {
    let err = check_rule(&rule(&[], "(.*)", "x"), &[]).unwrap_err();
    assert!(err.message.contains("empty `command`"), "{}", err.message);
}

#[test]
fn check_rule_missing_binary_is_error() {
    // A binary that cannot exist on PATH must yield an Err, not abort the run.
    let r = rule(
        &["definitely-not-a-real-binary-xyz", "--version"],
        r"(\d+)",
        "x",
    );
    let err = check_rule(&r, &[]).unwrap_err();
    assert!(err.message.contains("failed to run"), "{}", err.message);
}

// The branches below need a command that actually runs and exits successfully
// before the regex / lockfile logic is reached, so they use `sh -c` and are
// Unix-gated. A genuine end-to-end happy/mismatch path (spawning a real
// `--version` binary) lives in tests/cli_crate_version.rs.

#[cfg(unix)]
fn echo_rule(stdout: &str, pattern: &str, crate_name: &str) -> CliCrateVersionRule {
    rule(
        &["sh", "-c", &format!("echo '{stdout}'")],
        pattern,
        crate_name,
    )
}

#[cfg(unix)]
#[test]
fn check_rule_malformed_regex_is_error() {
    // Command runs fine; the invalid pattern is what fails — and it must become
    // a diagnostic, not a panic.
    let r = echo_rule("1.2.3", "(", "x");
    let err = check_rule(&r, &[]).unwrap_err();
    assert!(
        err.message.contains("invalid regex pattern"),
        "{}",
        err.message
    );
}

#[cfg(unix)]
#[test]
fn check_rule_crate_absent_from_lockfile_is_error() {
    // Pattern matches the output, but the crate isn't in the (empty) lockfile.
    let r = echo_rule("1.2.3", r"(\d+\.\d+\.\d+)", "missing");
    let err = check_rule(&r, &[]).unwrap_err();
    assert!(
        err.message.contains("not found in Cargo.lock"),
        "{}",
        err.message
    );
}

#[cfg(unix)]
#[test]
fn check_rule_pattern_without_capture_group_is_error() {
    // `\d+` has no group 1, so `extract_version` returns None and the rule
    // reports a "did not match" error rather than silently passing.
    let r = echo_rule("1.2.3", r"\d+", "x");
    let err = check_rule(&r, &[("x".into(), "1.2.3".into())]).unwrap_err();
    assert!(err.message.contains("did not match"), "{}", err.message);
}

#[cfg(unix)]
#[test]
fn check_rule_compares_versions_as_plain_strings() {
    // Version comparison is plain string equality, so `1.0` (CLI) and `1.0.0`
    // (lockfile) are a mismatch. Pin that so the (intentional) behavior is a
    // documented decision, not a surprise.
    let r = echo_rule("1.0", r"(\d+(?:\.\d+)+)", "x");
    let d = check_rule(&r, &[("x".into(), "1.0.0".into())])
        .unwrap()
        .expect("non-semver-equal versions are a mismatch finding");
    assert!(
        d.message.contains("1.0") && d.message.contains("1.0.0"),
        "{}",
        d.message
    );
}

#[cfg(unix)]
#[test]
fn check_rule_command_exits_nonzero_is_error() {
    // A command that runs but exits non-zero must surface as a diagnostic, not
    // a silent pass or an abort.
    let r = rule(&["sh", "-c", "exit 1"], r"(\d+)", "x");
    let err = check_rule(&r, &[]).unwrap_err();
    assert!(
        err.message.contains("exited unsuccessfully"),
        "{}",
        err.message
    );
}
