//! Resolver-backed unused-dependencies check.
//!
//! For each workspace member, the lint compares the declared deps in its
//! `Cargo.toml` against the set of crate names appearing in the resolver's
//! per-crate reference index. A dep is flagged unused if its
//! underscore-normalized name doesn't appear in that set.
//!
//! Inputs come from two sources:
//!
//! - **Cargo.toml** — for the declared dep list (resolver doesn't model
//!   dependencies, just declared items and references).
//! - **`Workspace::references_from`** — for the canonical-path set the
//!   crate touches (use statements + regular code paths + macro-body refs).
//!
//! Known limitations (documented in tests/cases/unused-deps/):
//!
//! - `build.rs`-generated code, `*-sys` link-only deps, and feature-plumbing
//!   deps still produce false positives; the existing `ignore` config knob
//!   suppresses them.
//! - Deps used only via fully-qualified paths inside proc-macro bodies the
//!   resolver doesn't expand (see `crates/syn-workspace/src/plugins/`).

use crate::config::UnusedDepsConfig;
use crate::diagnostic::Diagnostic;
use crate::diagnostic::builder::at_crate;
use crate::diagnostic::{Applicability, Span, Suggestion};
use fs_err as fs;
use std::collections::{BTreeMap, HashSet};
use syn_workspace::Workspace;

pub const LINT: &str = crate::lints::LintId::UnusedDeps.id();

pub fn check(config: &UnusedDepsConfig, workspace: &Workspace) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    for krate in workspace.members() {
        let cargo_path = krate.manifest_dir.join("Cargo.toml");
        if !cargo_path.exists() {
            continue;
        }

        let content = match fs::read_to_string(&cargo_path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("failed to read {}: {e}", cargo_path.display());
                std::process::exit(1);
            }
        };
        let doc: toml::Value = match content.parse() {
            Ok(d) => d,
            Err(e) => {
                eprintln!("failed to parse {}: {e}", cargo_path.display());
                std::process::exit(1);
            }
        };

        let deps = collect_deps_from_toml(&doc, &config.ignore);
        if deps.is_empty() {
            continue;
        }

        let referenced_crates = referenced_crate_names(workspace, krate);

        let unused = find_unused_deps(deps, &referenced_crates);
        if unused.is_empty() {
            continue;
        }

        let n = unused.len();
        // Normalize path separators in the message body. Spans get
        // renderer-normalized to forward slash, but free-form message text
        // embeds the path directly — without this, Windows snapshots
        // diverge from macOS/Linux ones.
        let cargo_path_str = cargo_path.display().to_string().replace('\\', "/");
        let mut builder = at_crate(
            LINT,
            format!(
                "{n} possibly unused dependenc{} in {cargo_path_str}",
                if n == 1 { "y" } else { "ies" },
            ),
            krate.manifest_dir.clone(),
        );
        for entry in &unused {
            builder = builder.help(format!("[{}] {}", entry.section, entry.original_name));
            if let Some(s) = build_delete_suggestion(&content, &cargo_path, entry) {
                builder = builder.suggestion(s);
            }
        }
        diagnostics.push(
            builder
                .note("build.rs-generated code, *-sys link-only deps, and feature-plumbing-only deps may still cause false positives")
                .note("verify by removing the dep and running `cargo build --all-targets`")
                .note("if the build breaks, add the dep to [unused-deps] ignore in your config")
                .build(),
        );
    }

    diagnostics
}

/// Build a `MachineApplicable` suggestion that deletes the entire dep
/// line (including the trailing newline) from the Cargo.toml. Returns
/// `None` if the dep entry spans multiple lines (inline table that wraps)
/// — those are deferred to manual deletion to avoid swallowing the table
/// body.
fn build_delete_suggestion(
    content: &str,
    cargo_path: &std::path::Path,
    entry: &DepEntry,
) -> Option<Suggestion> {
    let (line_start, line_end, line_number) =
        crate::centralized_deps::locate_dep_entry(content, &entry.section, &entry.original_name)?;
    // Extend the end past the trailing CR/LF so we don't leave a stray
    // blank line behind. `locate_dep_entry` returns the line's content
    // range only (no EOL bytes), so consume up to `\r\n` (Windows) or
    // `\n` (Unix) here.
    let mut end = line_end;
    if end < content.len() && content.as_bytes()[end] == b'\r' {
        end += 1;
    }
    if end < content.len() && content.as_bytes()[end] == b'\n' {
        end += 1;
    }
    Some(Suggestion {
        span: Span {
            file: cargo_path.to_path_buf(),
            line_start: line_number,
            line_end: line_number,
            col_start: 1,
            col_end: 1,
            byte_start: line_start as u32,
            byte_end: end as u32,
        },
        message: format!("remove unused dependency `{}`", entry.original_name),
        replacement: String::new(),
        applicability: Applicability::MachineApplicable,
    })
}

/// All distinct crate names that appear as the leading segment of any
/// reference from `krate`. Names come back in in-code form (underscores),
/// matching the dep keys after their own normalization.
fn referenced_crate_names(workspace: &Workspace, krate: &syn_workspace::Crate) -> HashSet<String> {
    workspace
        .references_from_crate(krate)
        .map(|refs| {
            refs.iter()
                .filter_map(|p| p.crate_name().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// One dep entry seen in a member crate's Cargo.toml. Tracks the exact
/// section and original name so D3's structural fix can locate the line
/// for deletion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DepEntry {
    pub(crate) section: String,
    /// Name as written in the Cargo.toml (may contain hyphens).
    pub(crate) original_name: String,
    /// Cargo-form name with hyphens replaced by underscores. Matches the
    /// crate-name segment in `ResolvedPath`.
    pub(crate) normalized_name: String,
}

fn collect_deps_from_toml(doc: &toml::Value, ignore: &[String]) -> BTreeMap<String, Vec<DepEntry>> {
    let mut deps: BTreeMap<String, Vec<DepEntry>> = BTreeMap::new();
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(table) = doc.get(section).and_then(|v| v.as_table()) {
            for name in table.keys() {
                if ignore.iter().any(|i| i == name) {
                    continue;
                }
                let normalized = name.replace('-', "_");
                deps.entry(normalized.clone()).or_default().push(DepEntry {
                    section: section.to_string(),
                    original_name: name.clone(),
                    normalized_name: normalized,
                });
            }
        }
    }
    deps
}

fn find_unused_deps(
    deps: BTreeMap<String, Vec<DepEntry>>,
    referenced: &HashSet<String>,
) -> Vec<DepEntry> {
    deps.into_iter()
        .filter(|(normalized, _)| !referenced.contains(normalized))
        .flat_map(|(_, entries)| entries)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- collect_deps_from_toml ---

    #[test]
    fn collect_deps_basic() {
        let doc: toml::Value = r#"
            [dependencies]
            serde = "1"
            tokio = { workspace = true }
        "#
        .parse()
        .unwrap();
        let deps = collect_deps_from_toml(&doc, &[]);
        assert!(deps.contains_key("serde"));
        assert!(deps.contains_key("tokio"));
    }

    #[test]
    fn collect_deps_normalizes_hyphens() {
        let doc: toml::Value = r#"
            [dependencies]
            my-crate = "1"
        "#
        .parse()
        .unwrap();
        let deps = collect_deps_from_toml(&doc, &[]);
        assert!(deps.contains_key("my_crate"));
    }

    #[test]
    fn collect_deps_respects_ignore() {
        let doc: toml::Value = r#"
            [dependencies]
            serde = "1"
            prost = "0.12"
        "#
        .parse()
        .unwrap();
        let deps = collect_deps_from_toml(&doc, &["prost".into()]);
        assert!(deps.contains_key("serde"));
        assert!(!deps.contains_key("prost"));
    }

    #[test]
    fn collect_deps_all_sections() {
        let doc: toml::Value = r#"
            [dependencies]
            a = "1"
            [dev-dependencies]
            b = "1"
            [build-dependencies]
            c = "1"
        "#
        .parse()
        .unwrap();
        let deps = collect_deps_from_toml(&doc, &[]);
        assert_eq!(deps.len(), 3);
        assert_eq!(deps["a"][0].section, "dependencies");
        assert_eq!(deps["b"][0].section, "dev-dependencies");
        assert_eq!(deps["c"][0].section, "build-dependencies");
    }

    // --- find_unused_deps ---

    fn entry(section: &str, name: &str) -> DepEntry {
        DepEntry {
            section: section.into(),
            original_name: name.into(),
            normalized_name: name.replace('-', "_"),
        }
    }

    #[test]
    fn find_unused_all_used() {
        let mut deps = BTreeMap::new();
        deps.insert("serde".into(), vec![entry("dependencies", "serde")]);
        let mut refs = HashSet::new();
        refs.insert("serde".into());
        assert!(find_unused_deps(deps, &refs).is_empty());
    }

    #[test]
    fn find_unused_none_used() {
        let mut deps = BTreeMap::new();
        deps.insert("serde".into(), vec![entry("dependencies", "serde")]);
        let refs = HashSet::new();
        let unused = find_unused_deps(deps, &refs);
        assert_eq!(unused, vec![entry("dependencies", "serde")]);
    }

    #[test]
    fn find_unused_partial() {
        let mut deps = BTreeMap::new();
        deps.insert("serde".into(), vec![entry("dependencies", "serde")]);
        deps.insert("rand".into(), vec![entry("dependencies", "rand")]);
        let mut refs = HashSet::new();
        refs.insert("serde".into());
        let unused = find_unused_deps(deps, &refs);
        assert_eq!(unused, vec![entry("dependencies", "rand")]);
    }

    // --- build_delete_suggestion: EOL handling ---

    #[test]
    fn delete_consumes_lf_after_dep_line() {
        let content = "[dependencies]\nrand = \"0.8\"\nfoo = \"1\"\n";
        let path = std::path::PathBuf::from("/tmp/Cargo.toml");
        let s = build_delete_suggestion(content, &path, &entry("dependencies", "rand")).unwrap();
        let start = s.span.byte_start as usize;
        let end = s.span.byte_end as usize;
        assert_eq!(&content[start..end], "rand = \"0.8\"\n");
    }

    #[test]
    fn delete_consumes_crlf_after_dep_line() {
        // Regression: on Windows, dep lines are terminated by `\r\n`. The
        // deletion must consume both bytes so we don't leave behind a stray
        // `\r` that produces a blank line.
        let content = "[dependencies]\r\nrand = \"0.8\"\r\nfoo = \"1\"\r\n";
        let path = std::path::PathBuf::from("/tmp/Cargo.toml");
        let s = build_delete_suggestion(content, &path, &entry("dependencies", "rand")).unwrap();
        let start = s.span.byte_start as usize;
        let end = s.span.byte_end as usize;
        assert_eq!(&content[start..end], "rand = \"0.8\"\r\n");
    }
}
