use crate::Issue;
use crate::config::FreshnessConfig;
use globset::Glob;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub fn check(config: &FreshnessConfig) -> Vec<Issue> {
    if std::env::var("CI").is_ok() {
        return Vec::new();
    }

    check_with_root(config, Path::new("."))
}

fn check_with_root(config: &FreshnessConfig, root: &Path) -> Vec<Issue> {
    let mut issues = Vec::new();

    for rule in &config.rules {
        let tracked_files = find_files_matching(root, &rule.glob);

        for file in &tracked_files {
            let file_mtime = match mtime(file) {
                Some(t) => t,
                None => continue,
            };

            let parent = file.parent().unwrap_or(Path::new("."));
            let parent_dir = if parent == Path::new("") {
                Path::new(".")
            } else {
                parent
            };

            let dep_files = find_deps_in_dir(parent_dir, &rule.depends_on);
            let newest_dep = dep_files.iter().filter_map(|p| mtime(p)).max();

            if let Some(newest) = newest_dep
                && newest > file_mtime
            {
                issues.push(Issue {
                    title: format!("Review {}", file.display()),
                    details: vec![
                        format!(
                            "Source files matching {} in subtree are newer",
                            rule.depends_on
                        ),
                        "Run 'workspace-lint done' when done".to_string(),
                    ],
                });
            }
        }
    }

    issues
}

pub fn mark_done(config: &FreshnessConfig) {
    let now = SystemTime::now();
    let times = std::fs::FileTimes::new().set_modified(now);

    for rule in &config.rules {
        let files = find_files_matching(Path::new("."), &rule.glob);
        for file in &files {
            let f = match fs_err::File::options().write(true).open(file) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("failed to open {}: {e}", file.display());
                    continue;
                }
            };
            if let Err(e) = f.set_times(times) {
                eprintln!("failed to touch {}: {e}", file.display());
            }
        }
    }
}

/// Find all tracked files matching a glob pattern, walking with .gitignore respect.
fn find_files_matching(root: &Path, pattern: &str) -> Vec<PathBuf> {
    let glob = Glob::new(pattern).unwrap_or_else(|e| {
        eprintln!("invalid glob pattern '{pattern}': {e}");
        std::process::exit(1);
    });
    let matcher = glob.compile_matcher();

    let mut results = Vec::new();
    for entry in ignore::WalkBuilder::new(root).build().flatten() {
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path().strip_prefix(root).unwrap_or(entry.path());
        if matcher.is_match(path) {
            results.push(entry.into_path());
        }
    }
    results
}

/// Find dependency files in a directory matching a glob pattern.
fn find_deps_in_dir(dir: &Path, pattern: &str) -> Vec<PathBuf> {
    let glob = Glob::new(pattern).unwrap_or_else(|e| {
        eprintln!("invalid depends-on pattern '{pattern}': {e}");
        std::process::exit(1);
    });
    let matcher = glob.compile_matcher();

    let mut results = Vec::new();
    for entry in ignore::WalkBuilder::new(dir).build().flatten() {
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let rel = match entry.path().strip_prefix(dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if matcher.is_match(rel) {
            results.push(entry.into_path());
        }
    }
    results
}

fn mtime(path: &Path) -> Option<SystemTime> {
    fs_err::metadata(path).ok()?.modified().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FreshnessRule;
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

        let files = find_files_matching(tmp.path(), "CLAUDE.md");
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

        let files = find_files_matching(tmp.path(), "**/CLAUDE.md");
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn find_files_matching_no_match() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("readme.md"), "").unwrap();

        let files = find_files_matching(tmp.path(), "CLAUDE.md");
        assert!(files.is_empty());
    }

    // --- find_deps_in_dir ---

    #[test]
    fn find_deps_basic() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("lib.rs"), "").unwrap();
        std::fs::write(tmp.path().join("main.rs"), "").unwrap();
        std::fs::write(tmp.path().join("readme.md"), "").unwrap();

        let deps = find_deps_in_dir(tmp.path(), "*.rs");
        assert_eq!(deps.len(), 2);
    }

    #[test]
    fn find_deps_recursive() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("src");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("lib.rs"), "").unwrap();

        let deps = find_deps_in_dir(tmp.path(), "**/*.rs");
        assert_eq!(deps.len(), 1);
    }

    // --- check_with_root (integration) ---

    #[test]
    fn fresh_file_no_issue() {
        let tmp = TempDir::new().unwrap();
        // Create a source file first (older)
        std::fs::write(tmp.path().join("lib.rs"), "fn foo() {}").unwrap();

        // Small delay to ensure different mtime
        std::thread::sleep(Duration::from_millis(50));

        // Create the tracked file after (newer)
        std::fs::write(tmp.path().join("CLAUDE.md"), "# doc").unwrap();

        let config = make_config(vec![("CLAUDE.md", "*.rs")]);
        let issues = check_with_root(&config, tmp.path());
        assert!(issues.is_empty());
    }

    #[test]
    fn stale_file_produces_issue() {
        let tmp = TempDir::new().unwrap();
        // Create tracked file first
        std::fs::write(tmp.path().join("CLAUDE.md"), "# doc").unwrap();

        // Make tracked file old
        let old = SystemTime::now() - Duration::from_secs(100);
        set_mtime(&tmp.path().join("CLAUDE.md"), old);

        // Create source file (newer) after
        std::fs::write(tmp.path().join("lib.rs"), "fn foo() {}").unwrap();

        let config = make_config(vec![("CLAUDE.md", "*.rs")]);
        let issues = check_with_root(&config, tmp.path());
        assert_eq!(issues.len(), 1);
        assert!(issues[0].title.contains("CLAUDE.md"));
    }

    #[test]
    fn no_deps_no_issue() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("CLAUDE.md"), "# doc").unwrap();
        // No .rs files exist

        let config = make_config(vec![("CLAUDE.md", "*.rs")]);
        let issues = check_with_root(&config, tmp.path());
        assert!(issues.is_empty());
    }

    #[test]
    fn ci_env_skips_check() {
        // The public check() function returns empty when CI is set.
        // We test check_with_root directly which doesn't check CI,
        // confirming the CI check is only in the public wrapper.
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("CLAUDE.md"), "# doc").unwrap();
        let old = SystemTime::now() - Duration::from_secs(100);
        set_mtime(&tmp.path().join("CLAUDE.md"), old);
        std::fs::write(tmp.path().join("lib.rs"), "fn foo() {}").unwrap();

        // check_with_root finds the issue regardless of CI
        let config = make_config(vec![("CLAUDE.md", "*.rs")]);
        let issues = check_with_root(&config, tmp.path());
        assert_eq!(issues.len(), 1);
    }
}
