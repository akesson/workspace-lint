use super::*;
use tempfile::TempDir;

#[test]
fn missing_paths_reports_only_absent() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("present.rs"), "").unwrap();
    // The listing claims both are tracked; only `gone.rs` is absent on disk.
    let missing = missing_paths("present.rs\0gone.rs\0", tmp.path());
    assert_eq!(missing, vec!["gone.rs"]);
}

#[test]
fn missing_paths_handles_unicode_and_spaces_verbatim() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("café.rs"), "").unwrap();
    // `-z` means no git quoting; the present unicode file is seen, the absent
    // spaced path is reported as-is.
    let missing = missing_paths("café.rs\0a b.rs\0", tmp.path());
    assert_eq!(missing, vec!["a b.rs"]);
}

#[test]
fn missing_paths_ignores_empty_segments() {
    let tmp = TempDir::new().unwrap();
    assert!(missing_paths("", tmp.path()).is_empty());
    std::fs::write(tmp.path().join("x.rs"), "").unwrap();
    // A trailing NUL must not become a phantom empty-path finding.
    assert!(missing_paths("x.rs\0", tmp.path()).is_empty());
}

#[test]
fn build_diagnostics_shape() {
    let ds = build_diagnostics(&["gone.rs"]);
    assert_eq!(ds.len(), 1);
    assert_eq!(ds[0].lint, LintId::StaleGitIndex.id());
    assert!(ds[0].message.contains("gone.rs"), "{}", ds[0].message);
    assert!(ds[0].helps.iter().any(|h| h.contains("git rm gone.rs")));
}
