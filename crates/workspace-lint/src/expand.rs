use crate::config::ExpandConfig;
use fs_err as fs;
use globset::Glob;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) fn run(config: &ExpandConfig) {
    run_with_root(config, Path::new("."));
}

fn run_with_root(config: &ExpandConfig, root: &Path) {
    for rule in &config.rules {
        let (program, args) = rule.command.split_first().unwrap_or_else(|| {
            eprintln!("expand: command must not be empty");
            std::process::exit(2);
        });

        let output = Command::new(program)
            .args(args)
            .output()
            .unwrap_or_else(|e| {
                eprintln!("expand: failed to run `{}`: {e}", rule.command.join(" "));
                std::process::exit(2);
            });

        if !output.status.success() {
            eprintln!(
                "expand: `{}` failed: {}",
                rule.command.join(" "),
                String::from_utf8_lossy(&output.stderr)
            );
            std::process::exit(2);
        }

        let raw = strip_ansi_escapes::strip(&output.stdout);
        let stdout = String::from_utf8_lossy(&raw);
        let body = format!("```\n{}```\n", stdout);

        let start_marker = format!("<!-- {}_START -->", rule.marker);
        let end_marker = format!("<!-- {}_END -->", rule.marker);

        let files = find_files_matching(root, &rule.glob);
        if files.is_empty() {
            eprintln!(
                "expand: no files matching `{}` for marker {}",
                rule.glob, rule.marker
            );
            continue;
        }

        for file in &files {
            let content = fs::read_to_string(file).unwrap_or_else(|e| {
                eprintln!("expand: failed to read {}: {e}", file.display());
                std::process::exit(2);
            });

            let Some(start) = content.find(&start_marker) else {
                eprintln!("expand: {}: missing {start_marker}", file.display());
                std::process::exit(2);
            };
            let Some(end) = content.find(&end_marker) else {
                eprintln!("expand: {}: missing {end_marker}", file.display());
                std::process::exit(2);
            };

            let new_content = format!(
                "{}{start_marker}\n{body}{end_marker}\n{}",
                &content[..start],
                &content[end + end_marker.len()..].trim_start_matches('\n'),
            );

            if new_content == content {
                continue;
            }

            fs::write(file, &new_content).unwrap_or_else(|e| {
                eprintln!("expand: failed to write {}: {e}", file.display());
                std::process::exit(2);
            });

            eprintln!(
                "expand: updated {} (marker {})",
                file.display(),
                rule.marker
            );

            if rule.auto_stage {
                // `Path::new(".")` preserves this site's historical cwd-relative
                // staging; the scrub in `git::command` is what matters here.
                let status = crate::git::command(Path::new("."))
                    .args(["add", &file.to_string_lossy()])
                    .status()
                    .expect("failed to run `git add`");

                if !status.success() {
                    eprintln!("expand: git add {} failed", file.display());
                    std::process::exit(2);
                }
            }
        }
    }
}

#[cfg(test)]
fn replace_marker(
    content: &str,
    start_marker: &str,
    end_marker: &str,
    body: &str,
) -> Result<String, String> {
    let Some(start) = content.find(start_marker) else {
        return Err(format!("missing {start_marker}"));
    };
    let Some(end) = content.find(end_marker) else {
        return Err(format!("missing {end_marker}"));
    };

    Ok(format!(
        "{}{start_marker}\n{body}{end_marker}\n{}",
        &content[..start],
        &content[end + end_marker.len()..].trim_start_matches('\n'),
    ))
}

fn find_files_matching(root: &Path, pattern: &str) -> Vec<PathBuf> {
    let glob = Glob::new(pattern).unwrap_or_else(|e| {
        eprintln!("expand: invalid glob pattern '{pattern}': {e}");
        std::process::exit(2);
    });
    let matcher = glob.compile_matcher();

    let mut results = Vec::new();
    for entry in ignore::WalkBuilder::new(root).build().flatten() {
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let rel = entry.path().strip_prefix(root).unwrap_or(entry.path());
        if matcher.is_match(rel) {
            results.push(entry.into_path());
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    fn markers(name: &str) -> (String, String) {
        (
            format!("<!-- {name}_START -->"),
            format!("<!-- {name}_END -->"),
        )
    }

    #[test]
    fn replace_marker_basic() {
        let content = "before\n<!-- FOO_START -->\nold stuff\n<!-- FOO_END -->\nafter\n";
        let (s, e) = markers("FOO");
        let result = replace_marker(content, &s, &e, "```\nnew\n```\n").unwrap();
        assert!(result.contains("```\nnew\n```\n"));
        assert!(result.starts_with("before\n"));
        assert!(result.ends_with("after\n"));
        assert!(!result.contains("old stuff"));
    }

    #[test]
    fn replace_marker_no_change_when_same() {
        let body = "```\nstuff\n```\n";
        let content = format!("<!-- M_START -->\n{body}<!-- M_END -->\n");
        let (s, e) = markers("M");
        let result = replace_marker(&content, &s, &e, body).unwrap();
        assert_eq!(result, content);
    }

    #[test]
    fn replace_marker_missing_start() {
        let content = "no markers here\n<!-- FOO_END -->\n";
        let (s, e) = markers("FOO");
        let err = replace_marker(content, &s, &e, "body").unwrap_err();
        assert!(err.contains("missing"));
        assert!(err.contains("START"));
    }

    #[test]
    fn replace_marker_missing_end() {
        let content = "<!-- FOO_START -->\nno end\n";
        let (s, e) = markers("FOO");
        let err = replace_marker(content, &s, &e, "body").unwrap_err();
        assert!(err.contains("missing"));
        assert!(err.contains("END"));
    }

    #[test]
    fn replace_marker_preserves_surrounding() {
        let content = "header\n<!-- X_START -->\nold\n<!-- X_END -->\nfooter\n";
        let (s, e) = markers("X");
        let result = replace_marker(content, &s, &e, "new\n").unwrap();
        assert!(result.starts_with("header\n"));
        assert!(result.ends_with("footer\n"));
    }

    #[test]
    fn replace_marker_multiline_body() {
        let content = "<!-- M_START -->\n<!-- M_END -->\n";
        let (s, e) = markers("M");
        let body = "line1\nline2\nline3\n";
        let result = replace_marker(content, &s, &e, body).unwrap();
        assert!(result.contains("line1\nline2\nline3\n"));
    }

    // --- run_with_root (end-to-end with subprocess) ---

    use crate::config::ExpandRule;
    use tempfile::TempDir;

    fn write(dir: &Path, name: &str, content: &str) {
        std::fs::write(dir.join(name), content).unwrap();
    }

    #[test]
    fn run_rewrites_file_with_command_output() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "DOC.md",
            "header\n<!-- VERSION_START -->\nold\n<!-- VERSION_END -->\nfooter\n",
        );

        let config = ExpandConfig {
            rules: vec![ExpandRule {
                command: vec!["cargo".into(), "--version".into()],
                glob: "DOC.md".into(),
                marker: "VERSION".into(),
                auto_stage: false,
            }],
        };

        run_with_root(&config, tmp.path());

        let result = std::fs::read_to_string(tmp.path().join("DOC.md")).unwrap();
        assert!(result.starts_with("header\n"));
        assert!(result.ends_with("footer\n"));
        assert!(result.contains("cargo "));
        assert!(!result.contains("old"));
    }

    #[test]
    fn run_noop_when_content_already_matches() {
        let tmp = TempDir::new().unwrap();
        // Pre-populate with the body the command would produce.
        let initial = "<!-- ECHO_START -->\n```\nhi\n```\n<!-- ECHO_END -->\n";
        write(tmp.path(), "X.md", initial);
        let before_mtime = std::fs::metadata(tmp.path().join("X.md"))
            .unwrap()
            .modified()
            .unwrap();

        // Use `printf` (POSIX) — falls back to no test on non-unix.
        #[cfg(unix)]
        {
            let config = ExpandConfig {
                rules: vec![ExpandRule {
                    command: vec!["printf".into(), "hi\n".into()],
                    glob: "X.md".into(),
                    marker: "ECHO".into(),
                    auto_stage: false,
                }],
            };
            run_with_root(&config, tmp.path());

            let after = std::fs::read_to_string(tmp.path().join("X.md")).unwrap();
            assert_eq!(after, initial);
            let after_mtime = std::fs::metadata(tmp.path().join("X.md"))
                .unwrap()
                .modified()
                .unwrap();
            assert_eq!(before_mtime, after_mtime, "file should not be rewritten");
        }
    }

    #[test]
    fn run_skips_when_no_matching_files() {
        let tmp = TempDir::new().unwrap();
        // No file matches the glob.
        let config = ExpandConfig {
            rules: vec![ExpandRule {
                command: vec!["cargo".into(), "--version".into()],
                glob: "NEVER_MATCHES.md".into(),
                marker: "X".into(),
                auto_stage: false,
            }],
        };
        // Should print a warning and continue without panicking.
        run_with_root(&config, tmp.path());
    }
}
