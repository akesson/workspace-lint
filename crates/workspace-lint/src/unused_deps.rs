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
use fs_err as fs;
use std::collections::{BTreeMap, HashSet};
use syn_workspace::Workspace;

pub const LINT: &str = "workspace-lint::unused-deps";

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
        for label in unused {
            builder = builder.help(label);
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

fn collect_deps_from_toml(doc: &toml::Value, ignore: &[String]) -> BTreeMap<String, Vec<String>> {
    let mut deps: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(table) = doc.get(section).and_then(|v| v.as_table()) {
            for name in table.keys() {
                if ignore.iter().any(|i| i == name) {
                    continue;
                }
                let normalized = name.replace('-', "_");
                deps.entry(normalized)
                    .or_default()
                    .push(format!("[{section}] {name}"));
            }
        }
    }
    deps
}

fn find_unused_deps(
    deps: BTreeMap<String, Vec<String>>,
    referenced: &HashSet<String>,
) -> Vec<String> {
    deps.into_iter()
        .filter(|(normalized, _)| !referenced.contains(normalized))
        .flat_map(|(_, labels)| labels)
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
        assert!(deps["a"][0].contains("[dependencies]"));
        assert!(deps["b"][0].contains("[dev-dependencies]"));
        assert!(deps["c"][0].contains("[build-dependencies]"));
    }

    // --- find_unused_deps ---

    #[test]
    fn find_unused_all_used() {
        let mut deps = BTreeMap::new();
        deps.insert("serde".into(), vec!["[dependencies] serde".into()]);
        let mut refs = HashSet::new();
        refs.insert("serde".into());
        assert!(find_unused_deps(deps, &refs).is_empty());
    }

    #[test]
    fn find_unused_none_used() {
        let mut deps = BTreeMap::new();
        deps.insert("serde".into(), vec!["[dependencies] serde".into()]);
        let refs = HashSet::new();
        let unused = find_unused_deps(deps, &refs);
        assert_eq!(unused, vec!["[dependencies] serde"]);
    }

    #[test]
    fn find_unused_partial() {
        let mut deps = BTreeMap::new();
        deps.insert("serde".into(), vec!["[dependencies] serde".into()]);
        deps.insert("rand".into(), vec!["[dependencies] rand".into()]);
        let mut refs = HashSet::new();
        refs.insert("serde".into());
        let unused = find_unused_deps(deps, &refs);
        assert_eq!(unused, vec!["[dependencies] rand"]);
    }
}
