use fs_err as fs;
use std::path::{Path, PathBuf};

pub fn extract_member_patterns(root: &toml::Value) -> Vec<String> {
    root.get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

pub fn expand_member_patterns(patterns: &[String]) -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    for pattern in patterns {
        if pattern.contains('*') {
            // Simple glob: "crates/server/*" → list entries in "crates/server/"
            let parent = pattern.trim_end_matches("/*").trim_end_matches("\\*");
            let parent_path = Path::new(parent);
            if let Ok(entries) = fs::read_dir(parent_path) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() && path.join("Cargo.toml").exists() {
                        dirs.push(path);
                    }
                }
            }
        } else {
            let path = Path::new(pattern);
            if path.is_dir() {
                dirs.push(path.to_path_buf());
            }
        }
    }

    dirs.sort();
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // --- extract_member_patterns ---

    #[test]
    fn extract_members_basic() {
        let toml: toml::Value = r#"
            [workspace]
            members = ["crates/a", "crates/b"]
        "#
        .parse()
        .unwrap();
        assert_eq!(extract_member_patterns(&toml), vec!["crates/a", "crates/b"]);
    }

    #[test]
    fn extract_members_empty_array() {
        let toml: toml::Value = r#"
            [workspace]
            members = []
        "#
        .parse()
        .unwrap();
        assert!(extract_member_patterns(&toml).is_empty());
    }

    #[test]
    fn extract_members_no_workspace_key() {
        let toml: toml::Value = r#"
            [package]
            name = "foo"
        "#
        .parse()
        .unwrap();
        assert!(extract_member_patterns(&toml).is_empty());
    }

    // --- expand_member_patterns ---

    #[test]
    fn expand_literal_member() {
        let tmp = TempDir::new().unwrap();
        let crate_dir = tmp.path().join("my-crate");
        std::fs::create_dir(&crate_dir).unwrap();
        std::fs::write(
            crate_dir.join("Cargo.toml"),
            "[package]\nname = \"my-crate\"",
        )
        .unwrap();

        let patterns = vec![crate_dir.display().to_string()];
        let dirs = expand_member_patterns(&patterns);
        assert_eq!(dirs, vec![crate_dir]);
    }

    #[test]
    fn expand_glob_finds_crates() {
        let tmp = TempDir::new().unwrap();
        let parent = tmp.path().join("crates");
        std::fs::create_dir(&parent).unwrap();

        // crate with Cargo.toml
        let a = parent.join("alpha");
        std::fs::create_dir(&a).unwrap();
        std::fs::write(a.join("Cargo.toml"), "").unwrap();

        // crate with Cargo.toml
        let b = parent.join("beta");
        std::fs::create_dir(&b).unwrap();
        std::fs::write(b.join("Cargo.toml"), "").unwrap();

        let patterns = vec![format!("{}/*", parent.display())];
        let dirs = expand_member_patterns(&patterns);
        assert_eq!(dirs.len(), 2);
        assert!(dirs.contains(&a));
        assert!(dirs.contains(&b));
    }

    #[test]
    fn expand_glob_skips_non_crate_dirs() {
        let tmp = TempDir::new().unwrap();
        let parent = tmp.path().join("crates");
        std::fs::create_dir(&parent).unwrap();

        // dir without Cargo.toml
        std::fs::create_dir(parent.join("not-a-crate")).unwrap();

        // a regular file (not a dir)
        std::fs::write(parent.join("README.md"), "hi").unwrap();

        let patterns = vec![format!("{}/*", parent.display())];
        let dirs = expand_member_patterns(&patterns);
        assert!(dirs.is_empty());
    }

    #[test]
    fn expand_empty_patterns() {
        let dirs = expand_member_patterns(&[]);
        assert!(dirs.is_empty());
    }
}
