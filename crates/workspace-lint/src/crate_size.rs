use crate::config::CrateSizeConfig;
use crate::diagnostic::Diagnostic;
use crate::diagnostic::builder::at_crate;
use globset::{Glob, GlobSetBuilder};
use std::path::Path;
use tokei::{Config as TokeiConfig, Languages};

pub const LINT: &str = crate::lints::LintId::CrateSize.id();

pub fn check(config: &CrateSizeConfig) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    for rule in &config.rules {
        let dirs = expand_glob(&rule.glob);
        let include_set = rule.include.as_ref().map(|patterns| {
            let mut builder = GlobSetBuilder::new();
            for p in patterns {
                builder.add(Glob::new(p).unwrap_or_else(|e| {
                    eprintln!("invalid include pattern '{p}': {e}");
                    std::process::exit(1);
                }));
            }
            builder.build().unwrap()
        });

        for dir in &dirs {
            let mut languages = Languages::new();
            languages.get_statistics(&[dir.as_str()], &[], &TokeiConfig::default());

            let mut total_code: usize = 0;
            for language in languages.values() {
                for report in &language.reports {
                    if let Some(ref gs) = include_set {
                        let name = report.name.file_name().unwrap_or_default();
                        if !gs.is_match(Path::new(name)) {
                            continue;
                        }
                    }
                    total_code += report.stats.code;
                }
            }

            if total_code > rule.max_code_lines {
                diagnostics.push(
                    at_crate(
                        LINT,
                        format!(
                            "crate exceeds {} code lines ({total_code})",
                            rule.max_code_lines
                        ),
                        dir.clone(),
                    )
                    .help("split the crate into smaller, more focused crates")
                    .note(format!(
                        "configured by [[crate-size.rules]] glob = \"{}\"",
                        rule.glob
                    ))
                    .build(),
                );
            }
        }
    }

    diagnostics
}

/// Expand a glob pattern to matching directories.
fn expand_glob(pattern: &str) -> Vec<String> {
    let glob = Glob::new(pattern).unwrap_or_else(|e| {
        eprintln!("invalid crate-size glob '{pattern}': {e}");
        std::process::exit(1);
    });
    let matcher = glob.compile_matcher();

    // Walk top-level to find matching directories.
    // Support patterns like "crates/*" or "crates/web-*" by walking the parent.
    let parent = pattern
        .find(['*', '?', '['])
        .map(|pos| &pattern[..pattern[..pos].rfind('/').map(|i| i + 1).unwrap_or(0)])
        .unwrap_or(pattern);

    let parent_path = if parent.is_empty() {
        Path::new(".")
    } else {
        Path::new(parent)
    };

    let mut dirs = Vec::new();
    if let Ok(entries) = std::fs::read_dir(parent_path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let rel = path
                    .strip_prefix("./")
                    .unwrap_or(&path)
                    .display()
                    .to_string();
                if matcher.is_match(&rel) {
                    dirs.push(rel);
                }
            }
        }
    }

    dirs.sort();
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CrateSizeRule;
    use tempfile::TempDir;

    fn find_crate_violations(
        dir_line_counts: &[(String, usize)],
        rule: &CrateSizeRule,
    ) -> Vec<Diagnostic> {
        dir_line_counts
            .iter()
            .filter(|(_, count)| *count > rule.max_code_lines)
            .map(|(dir, count)| {
                at_crate(
                    LINT,
                    format!("crate exceeds {} code lines ({count})", rule.max_code_lines),
                    dir.clone(),
                )
                .build()
            })
            .collect()
    }

    fn rule(glob: &str, max: usize) -> CrateSizeRule {
        CrateSizeRule {
            glob: glob.into(),
            max_code_lines: max,
            include: None,
        }
    }

    // --- find_crate_violations ---

    #[test]
    fn no_violations_when_all_under_limit() {
        let counts = vec![("crates/a".into(), 100), ("crates/b".into(), 200)];
        let r = rule("crates/*", 500);
        assert!(find_crate_violations(&counts, &r).is_empty());
    }

    #[test]
    fn one_diagnostic_per_violation() {
        let counts = vec![("crates/a".into(), 600), ("crates/b".into(), 200)];
        let r = rule("crates/*", 500);
        let diags = find_crate_violations(&counts, &r);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("600"));
        assert_eq!(diags[0].lint, LINT);
    }

    #[test]
    fn multiple_violations_each_emit_diagnostic() {
        let counts = vec![
            ("crates/a".into(), 600),
            ("crates/b".into(), 900),
            ("crates/c".into(), 700),
        ];
        let r = rule("crates/*", 500);
        let diags = find_crate_violations(&counts, &r);
        assert_eq!(diags.len(), 3);
        let messages: Vec<&str> = diags.iter().map(|d| d.message.as_str()).collect();
        assert!(messages.iter().any(|m| m.contains("600")));
        assert!(messages.iter().any(|m| m.contains("900")));
        assert!(messages.iter().any(|m| m.contains("700")));
    }

    // --- expand_glob (tempdir) ---

    #[test]
    fn expand_glob_finds_dirs() {
        let tmp = TempDir::new().unwrap();
        let parent = tmp.path().join("crates");
        std::fs::create_dir(&parent).unwrap();
        std::fs::create_dir(parent.join("alpha")).unwrap();
        std::fs::create_dir(parent.join("beta")).unwrap();
        // a file, not a dir
        std::fs::write(parent.join("readme.md"), "").unwrap();

        let pattern = format!("{}/*", parent.display());
        let dirs = expand_glob(&pattern);
        assert_eq!(dirs.len(), 2);
    }

    #[test]
    fn expand_glob_empty_parent() {
        let tmp = TempDir::new().unwrap();
        let parent = tmp.path().join("empty");
        std::fs::create_dir(&parent).unwrap();

        let pattern = format!("{}/*", parent.display());
        let dirs = expand_glob(&pattern);
        assert!(dirs.is_empty());
    }

    #[test]
    fn expand_glob_nonexistent_parent() {
        let dirs = expand_glob("/nonexistent/path/*");
        assert!(dirs.is_empty());
    }
}
