use crate::Issue;
use crate::config::CliCrateVersionConfig;
use fs_err as fs;
use regex::Regex;
use std::process::Command;

pub fn check(config: &CliCrateVersionConfig) -> Vec<Issue> {
    let lock_packages = read_lock_packages();
    let mut issues = Vec::new();

    for rule in &config.rules {
        let (program, args) = rule.command.split_first().unwrap_or_else(|| {
            eprintln!("cli-crate-version: command must not be empty");
            std::process::exit(1);
        });

        let output = Command::new(program)
            .args(args)
            .output()
            .unwrap_or_else(|e| {
                eprintln!(
                    "cli-crate-version: failed to run `{}`: {e}",
                    rule.command.join(" ")
                );
                std::process::exit(1);
            });

        if !output.status.success() {
            eprintln!(
                "cli-crate-version: `{}` failed: {}",
                rule.command.join(" "),
                String::from_utf8_lossy(&output.stderr)
            );
            std::process::exit(1);
        }

        let raw = strip_ansi_escapes::strip(&output.stdout);
        let stdout = String::from_utf8_lossy(&raw);

        let re = Regex::new(&rule.pattern).unwrap_or_else(|e| {
            eprintln!(
                "cli-crate-version: invalid regex pattern `{}`: {e}",
                rule.pattern
            );
            std::process::exit(1);
        });

        let cli_version = re
            .captures(&stdout)
            .and_then(|caps| caps.get(1))
            .map(|m| m.as_str())
            .unwrap_or_else(|| {
                eprintln!(
                    "cli-crate-version: pattern `{}` did not match output of `{}`",
                    rule.pattern,
                    rule.command.join(" ")
                );
                std::process::exit(1);
            });

        let lock_version = lock_packages
            .iter()
            .find(|(name, _)| name == &rule.crate_name)
            .map(|(_, version)| version.as_str())
            .unwrap_or_else(|| {
                eprintln!(
                    "cli-crate-version: crate `{}` not found in Cargo.lock",
                    rule.crate_name
                );
                std::process::exit(1);
            });

        if cli_version != lock_version {
            issues.push(Issue {
                title: format!("Fix {} version mismatch", rule.crate_name),
                details: vec![format!(
                    "CLI reports {cli_version}, Cargo.lock has {lock_version}"
                )],
            });
        }
    }

    issues
}

fn read_lock_packages() -> Vec<(String, String)> {
    let content = fs::read_to_string("Cargo.lock").unwrap_or_else(|e| {
        eprintln!("failed to read Cargo.lock: {e}");
        std::process::exit(1);
    });
    parse_lock_packages(&content)
}

fn parse_lock_packages(content: &str) -> Vec<(String, String)> {
    let doc: toml::Value = content.parse().unwrap_or_else(|e| {
        eprintln!("failed to parse Cargo.lock: {e}");
        std::process::exit(1);
    });

    doc.get("package")
        .and_then(|p| p.as_array())
        .map(|packages| {
            packages
                .iter()
                .filter_map(|pkg| {
                    let name = pkg.get("name")?.as_str()?;
                    let version = pkg.get("version")?.as_str()?;
                    Some((name.to_string(), version.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
fn compare_version(
    cli_version: &str,
    crate_name: &str,
    lock_packages: &[(String, String)],
) -> Option<Issue> {
    let lock_version = lock_packages
        .iter()
        .find(|(name, _)| name == crate_name)
        .map(|(_, version)| version.as_str());

    let lock_version = lock_version?;

    if cli_version != lock_version {
        Some(Issue {
            title: format!("Fix {crate_name} version mismatch"),
            details: vec![format!(
                "CLI reports {cli_version}, Cargo.lock has {lock_version}"
            )],
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse_lock_packages ---

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
        let pkgs = parse_lock_packages(content);
        assert_eq!(pkgs.len(), 2);
        assert_eq!(pkgs[0], ("serde".into(), "1.0.200".into()));
        assert_eq!(pkgs[1], ("tokio".into(), "1.37.0".into()));
    }

    #[test]
    fn parse_lock_empty() {
        let pkgs = parse_lock_packages("");
        assert!(pkgs.is_empty());
    }

    #[test]
    fn parse_lock_no_package_key() {
        let content = r#"
[metadata]
foo = "bar"
"#;
        let pkgs = parse_lock_packages(content);
        assert!(pkgs.is_empty());
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
        let pkgs = parse_lock_packages(content);
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].0, "ok");
    }

    // --- compare_version ---

    #[test]
    fn compare_version_match() {
        let pkgs = vec![("wasm-bindgen".into(), "0.2.90".into())];
        assert!(compare_version("0.2.90", "wasm-bindgen", &pkgs).is_none());
    }

    #[test]
    fn compare_version_mismatch() {
        let pkgs = vec![("wasm-bindgen".into(), "0.2.90".into())];
        let issue = compare_version("0.2.89", "wasm-bindgen", &pkgs);
        assert!(issue.is_some());
        let issue = issue.unwrap();
        assert!(issue.title.contains("wasm-bindgen"));
        assert!(issue.details[0].contains("0.2.89"));
        assert!(issue.details[0].contains("0.2.90"));
    }

    #[test]
    fn compare_version_crate_not_in_lock() {
        let pkgs = vec![("serde".into(), "1.0".into())];
        assert!(compare_version("1.0", "missing-crate", &pkgs).is_none());
    }

    #[test]
    fn compare_version_empty_packages() {
        assert!(compare_version("1.0", "any", &[]).is_none());
    }

    #[test]
    fn compare_version_multiple_packages() {
        let pkgs = vec![
            ("alpha".into(), "1.0".into()),
            ("beta".into(), "2.0".into()),
            ("gamma".into(), "3.0".into()),
        ];
        assert!(compare_version("2.0", "beta", &pkgs).is_none());
        assert!(compare_version("999", "beta", &pkgs).is_some());
    }
}
