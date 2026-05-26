use crate::config::ExpandConfig;
use fs_err as fs;
use globset::Glob;
use std::path::PathBuf;
use std::process::Command;

pub fn run(config: &ExpandConfig) {
    for rule in &config.rules {
        let (program, args) = rule.command.split_first().unwrap_or_else(|| {
            eprintln!("expand: command must not be empty");
            std::process::exit(1);
        });

        let output = Command::new(program)
            .args(args)
            .output()
            .unwrap_or_else(|e| {
                eprintln!("expand: failed to run `{}`: {e}", rule.command.join(" "));
                std::process::exit(1);
            });

        if !output.status.success() {
            eprintln!(
                "expand: `{}` failed: {}",
                rule.command.join(" "),
                String::from_utf8_lossy(&output.stderr)
            );
            std::process::exit(1);
        }

        let raw = strip_ansi_escapes::strip(&output.stdout);
        let stdout = String::from_utf8_lossy(&raw);
        let body = format!("```\n{}```\n", stdout);

        let start_marker = format!("<!-- {}_START -->", rule.marker);
        let end_marker = format!("<!-- {}_END -->", rule.marker);

        let files = find_files_matching(&rule.glob);
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
                std::process::exit(1);
            });

            let Some(start) = content.find(&start_marker) else {
                eprintln!("expand: {}: missing {start_marker}", file.display());
                std::process::exit(1);
            };
            let Some(end) = content.find(&end_marker) else {
                eprintln!("expand: {}: missing {end_marker}", file.display());
                std::process::exit(1);
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
                std::process::exit(1);
            });

            eprintln!(
                "expand: updated {} (marker {})",
                file.display(),
                rule.marker
            );

            if rule.auto_stage {
                let status = Command::new("git")
                    .args(["add", &file.to_string_lossy()])
                    .status()
                    .expect("failed to run `git add`");

                if !status.success() {
                    eprintln!("expand: git add {} failed", file.display());
                    std::process::exit(1);
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

fn find_files_matching(pattern: &str) -> Vec<PathBuf> {
    let glob = Glob::new(pattern).unwrap_or_else(|e| {
        eprintln!("expand: invalid glob pattern '{pattern}': {e}");
        std::process::exit(1);
    });
    let matcher = glob.compile_matcher();

    let mut results = Vec::new();
    for entry in ignore::WalkBuilder::new(".").build().flatten() {
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path().strip_prefix("./").unwrap_or(entry.path());
        if matcher.is_match(path) {
            results.push(path.to_path_buf());
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
}
