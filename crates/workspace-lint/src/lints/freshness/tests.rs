use super::*;
use crate::config::{GlobPattern, Globs};
use std::time::Duration;
use tempfile::TempDir;

fn make_config(rules: Vec<(&str, &str)>) -> FreshnessConfig {
    FreshnessConfig {
        rules: rules
            .into_iter()
            .map(|(glob, depends_on)| FreshnessRule {
                glob: glob.into(),
                depends_on: depends_on.into(),
            })
            .collect(),
    }
}

fn set_mtime(path: &Path, time: SystemTime) {
    let f = std::fs::File::options().write(true).open(path).unwrap();
    let times = std::fs::FileTimes::new().set_modified(time);
    f.set_times(times).unwrap();
}

// --- find_files_matching ---

#[test]
fn find_files_matching_basic() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("CLAUDE.md"), "# doc").unwrap();
    std::fs::write(tmp.path().join("other.txt"), "hi").unwrap();

    let files = find_files_matching(tmp.path(), &GlobPattern::from("CLAUDE.md"));
    assert_eq!(files.len(), 1);
    assert!(files[0].ends_with("CLAUDE.md"));
}

#[test]
fn find_files_matching_glob() {
    let tmp = TempDir::new().unwrap();
    let sub = tmp.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("CLAUDE.md"), "").unwrap();
    std::fs::write(tmp.path().join("CLAUDE.md"), "").unwrap();

    let files = find_files_matching(tmp.path(), &GlobPattern::from("**/CLAUDE.md"));
    assert_eq!(files.len(), 2);
}

#[test]
fn find_files_matching_no_match() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("readme.md"), "").unwrap();

    let files = find_files_matching(tmp.path(), &GlobPattern::from("CLAUDE.md"));
    assert!(files.is_empty());
}

// --- find_deps_in_dir ---

#[test]
fn find_deps_basic() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("lib.rs"), "").unwrap();
    std::fs::write(tmp.path().join("main.rs"), "").unwrap();
    std::fs::write(tmp.path().join("readme.md"), "").unwrap();

    let deps = find_deps_in_dir(tmp.path(), &Globs::from("*.rs"));
    assert_eq!(deps.len(), 2);
}

#[test]
fn find_deps_recursive() {
    let tmp = TempDir::new().unwrap();
    let sub = tmp.path().join("src");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("lib.rs"), "").unwrap();

    let deps = find_deps_in_dir(tmp.path(), &Globs::from("**/*.rs"));
    assert_eq!(deps.len(), 1);
}

// --- check_with_root (integration) ---

#[test]
fn fresh_file_no_issue() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("lib.rs"), "fn foo() {}").unwrap();

    std::thread::sleep(Duration::from_millis(50));

    std::fs::write(tmp.path().join("CLAUDE.md"), "# doc").unwrap();

    let config = make_config(vec![("CLAUDE.md", "*.rs")]);
    let issues = check_with_root(&config, tmp.path());
    assert!(issues.is_empty());
}

#[test]
fn stale_file_produces_issue() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("CLAUDE.md"), "# doc").unwrap();

    let old = SystemTime::now() - Duration::from_secs(100);
    set_mtime(&tmp.path().join("CLAUDE.md"), old);

    std::fs::write(tmp.path().join("lib.rs"), "fn foo() {}").unwrap();

    let config = make_config(vec![("CLAUDE.md", "*.rs")]);
    let issues = check_with_root(&config, tmp.path());
    assert_eq!(issues.len(), 1);
    assert!(issues[0].message.contains("CLAUDE.md"));
}

#[test]
fn no_deps_no_issue() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("CLAUDE.md"), "# doc").unwrap();

    let config = make_config(vec![("CLAUDE.md", "*.rs")]);
    let issues = check_with_root(&config, tmp.path());
    assert!(issues.is_empty());
}

#[test]
fn ci_gate_skips_even_when_stale() {
    // A genuinely-stale tree: with the CI gate ON the lint must stay silent
    // (mtimes are meaningless after a checkout); with it OFF it must fire.
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("CLAUDE.md"), "# doc").unwrap();
    set_mtime(
        &tmp.path().join("CLAUDE.md"),
        SystemTime::now() - Duration::from_secs(100),
    );
    std::fs::write(tmp.path().join("lib.rs"), "fn foo() {}").unwrap();

    let config = make_config(vec![("CLAUDE.md", "*.rs")]);
    assert!(check_gated(&config, tmp.path(), true).is_empty());
    assert_eq!(check_gated(&config, tmp.path(), false).len(), 1);
}

#[test]
fn equal_mtime_is_fresh() {
    // Staleness is a strict `>` comparison: a dep with the *same* mtime as the
    // tracked file must not be reported.
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("CLAUDE.md"), "# doc").unwrap();
    std::fs::write(tmp.path().join("lib.rs"), "fn foo() {}").unwrap();
    let t = SystemTime::now() - Duration::from_secs(10);
    set_mtime(&tmp.path().join("CLAUDE.md"), t);
    set_mtime(&tmp.path().join("lib.rs"), t);

    let config = make_config(vec![("CLAUDE.md", "*.rs")]);
    assert!(check_with_root(&config, tmp.path()).is_empty());
}

// --- mark_done_with_root ---

#[test]
fn mark_done_touches_tracked_files() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("CLAUDE.md"), "# doc").unwrap();
    let old = SystemTime::now() - Duration::from_secs(3600);
    set_mtime(&tmp.path().join("CLAUDE.md"), old);

    let config = make_config(vec![("CLAUDE.md", "*.rs")]);
    mark_done_with_root(&config, tmp.path());

    let new_mtime = mtime(&tmp.path().join("CLAUDE.md")).unwrap();
    assert!(new_mtime > old);
}

#[test]
fn mark_done_no_matching_files_is_noop() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("readme.md"), "").unwrap();

    let config = make_config(vec![("CLAUDE.md", "*.rs")]);
    mark_done_with_root(&config, tmp.path());
    // No panic, no side effect on the unrelated file's existence.
    assert!(tmp.path().join("readme.md").exists());
}

#[test]
fn mark_done_multiple_rules_touches_all_matches() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("CLAUDE.md"), "").unwrap();
    std::fs::write(tmp.path().join("NOTES.md"), "").unwrap();
    let old = SystemTime::now() - Duration::from_secs(3600);
    set_mtime(&tmp.path().join("CLAUDE.md"), old);
    set_mtime(&tmp.path().join("NOTES.md"), old);

    let config = make_config(vec![("CLAUDE.md", "*.rs"), ("NOTES.md", "*.rs")]);
    mark_done_with_root(&config, tmp.path());

    assert!(mtime(&tmp.path().join("CLAUDE.md")).unwrap() > old);
    assert!(mtime(&tmp.path().join("NOTES.md")).unwrap() > old);
}
