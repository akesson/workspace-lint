//! Resolver-backed unused-dependencies check.
//!
//! For each workspace member, the lint compares the declared deps in its
//! `Cargo.toml` against the set of crate names appearing in the resolver's
//! per-crate reference index. A dep is flagged unused if its
//! underscore-normalized name doesn't appear in that set.
//!
//! Inputs come from two sources, both already loaded on the resolver:
//!
//! - **`Crate::declared_deps`** — the manifest's enumerated dep list across
//!   `[dependencies]`, `[dev-dependencies]`, and `[build-dependencies]`.
//! - **`Workspace::references_from_crate`** — for the canonical-path set the
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
use std::collections::{BTreeMap, HashSet};
use syn_workspace::Workspace;
use syn_workspace::manifest::{DeclaredDep, Manifest};

pub const LINT: &str = crate::lints::LintId::UnusedDeps.id();

pub fn check(config: &UnusedDepsConfig, workspace: &Workspace) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    for krate in workspace.members() {
        let manifest = krate.manifest();
        let deps = collect_deps(manifest, &config.ignore);
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
        let cargo_path_str = manifest.path().display().to_string().replace('\\', "/");
        let mut builder = at_crate(
            LINT,
            format!(
                "{n} possibly unused dependenc{} in {cargo_path_str}",
                if n == 1 { "y" } else { "ies" },
            ),
            krate.manifest_dir.clone(),
        );
        for entry in &unused {
            builder = builder.help(format!(
                "[{}] {}",
                entry.section.as_str(),
                entry.original_name
            ));
            if let Some(s) = build_delete_suggestion(manifest, entry) {
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
fn build_delete_suggestion(manifest: &Manifest, entry: &DeclaredDep) -> Option<Suggestion> {
    let location = manifest.locate_dep(entry.section, &entry.original_name)?;
    // Extend the end past the trailing CR/LF so we don't leave a stray
    // blank line behind. `locate_dep` returns the line's content range only
    // (no EOL bytes), so consume up to `\r\n` (Windows) or `\n` (Unix) here.
    let mut end = location.byte_end as usize;
    let bytes = manifest.raw().as_bytes();
    if end < bytes.len() && bytes[end] == b'\r' {
        end += 1;
    }
    if end < bytes.len() && bytes[end] == b'\n' {
        end += 1;
    }
    Some(Suggestion {
        span: Span {
            file: manifest.path().to_path_buf(),
            line_start: location.line,
            line_end: location.line,
            col_start: 1,
            col_end: 1,
            byte_start: location.byte_start,
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

/// Collect declared deps, applying the config's `ignore` filter. Groups
/// by normalized (underscore) name so a dep declared in both `dependencies`
/// and `dev-dependencies` is checked once.
fn collect_deps(manifest: &Manifest, ignore: &[String]) -> BTreeMap<String, Vec<DeclaredDep>> {
    let mut deps: BTreeMap<String, Vec<DeclaredDep>> = BTreeMap::new();
    for dep in manifest.declared_deps() {
        if ignore.iter().any(|i| i == &dep.original_name) {
            continue;
        }
        deps.entry(dep.normalized_name.clone())
            .or_default()
            .push(dep);
    }
    deps
}

fn find_unused_deps(
    deps: BTreeMap<String, Vec<DeclaredDep>>,
    referenced: &HashSet<String>,
) -> Vec<DeclaredDep> {
    deps.into_iter()
        .filter(|(normalized, _)| !referenced.contains(normalized))
        .flat_map(|(_, entries)| entries)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn_workspace::manifest::DepSection;

    fn parse_manifest(content: &str) -> Manifest {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("Cargo.toml");
        std::fs::write(&p, content).unwrap();
        // Keep the tempdir alive by leaking it — tests are short and run in
        // isolated processes, so the small leak is fine.
        let _ = Box::leak(Box::new(dir));
        Manifest::load(&p).unwrap()
    }

    fn entry(section: DepSection, name: &str) -> DeclaredDep {
        DeclaredDep {
            section,
            original_name: name.into(),
            normalized_name: name.replace('-', "_"),
        }
    }

    // --- collect_deps ---

    #[test]
    fn collect_deps_basic() {
        let m = parse_manifest(
            r#"
[dependencies]
serde = "1"
tokio = { workspace = true }
"#,
        );
        let deps = collect_deps(&m, &[]);
        assert!(deps.contains_key("serde"));
        assert!(deps.contains_key("tokio"));
    }

    #[test]
    fn collect_deps_normalizes_hyphens() {
        let m = parse_manifest(
            r#"
[dependencies]
my-crate = "1"
"#,
        );
        let deps = collect_deps(&m, &[]);
        assert!(deps.contains_key("my_crate"));
    }

    #[test]
    fn collect_deps_respects_ignore() {
        let m = parse_manifest(
            r#"
[dependencies]
serde = "1"
prost = "0.12"
"#,
        );
        let deps = collect_deps(&m, &["prost".into()]);
        assert!(deps.contains_key("serde"));
        assert!(!deps.contains_key("prost"));
    }

    #[test]
    fn collect_deps_all_sections() {
        let m = parse_manifest(
            r#"
[dependencies]
a = "1"
[dev-dependencies]
b = "1"
[build-dependencies]
c = "1"
"#,
        );
        let deps = collect_deps(&m, &[]);
        assert_eq!(deps.len(), 3);
        assert_eq!(deps["a"][0].section, DepSection::Dependencies);
        assert_eq!(deps["b"][0].section, DepSection::DevDependencies);
        assert_eq!(deps["c"][0].section, DepSection::BuildDependencies);
    }

    // --- find_unused_deps ---

    #[test]
    fn find_unused_all_used() {
        let mut deps = BTreeMap::new();
        deps.insert(
            "serde".into(),
            vec![entry(DepSection::Dependencies, "serde")],
        );
        let mut refs = HashSet::new();
        refs.insert("serde".into());
        assert!(find_unused_deps(deps, &refs).is_empty());
    }

    #[test]
    fn find_unused_none_used() {
        let mut deps = BTreeMap::new();
        deps.insert(
            "serde".into(),
            vec![entry(DepSection::Dependencies, "serde")],
        );
        let refs = HashSet::new();
        let unused = find_unused_deps(deps, &refs);
        assert_eq!(unused, vec![entry(DepSection::Dependencies, "serde")]);
    }

    #[test]
    fn find_unused_partial() {
        let mut deps = BTreeMap::new();
        deps.insert(
            "serde".into(),
            vec![entry(DepSection::Dependencies, "serde")],
        );
        deps.insert("rand".into(), vec![entry(DepSection::Dependencies, "rand")]);
        let mut refs = HashSet::new();
        refs.insert("serde".into());
        let unused = find_unused_deps(deps, &refs);
        assert_eq!(unused, vec![entry(DepSection::Dependencies, "rand")]);
    }

    // --- build_delete_suggestion: EOL handling ---

    #[test]
    fn delete_consumes_lf_after_dep_line() {
        let m = parse_manifest("[dependencies]\nrand = \"0.8\"\nfoo = \"1\"\n");
        let s = build_delete_suggestion(&m, &entry(DepSection::Dependencies, "rand")).unwrap();
        let start = s.span.byte_start as usize;
        let end = s.span.byte_end as usize;
        assert_eq!(&m.raw()[start..end], "rand = \"0.8\"\n");
    }

    #[test]
    fn delete_consumes_crlf_after_dep_line() {
        let m = parse_manifest("[dependencies]\r\nrand = \"0.8\"\r\nfoo = \"1\"\r\n");
        let s = build_delete_suggestion(&m, &entry(DepSection::Dependencies, "rand")).unwrap();
        let start = s.span.byte_start as usize;
        let end = s.span.byte_end as usize;
        assert_eq!(&m.raw()[start..end], "rand = \"0.8\"\r\n");
    }
}
