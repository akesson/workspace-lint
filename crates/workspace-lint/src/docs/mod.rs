//! Per-lint documentation, keyed on [`LintId`].
//!
//! The 12 real lints carry their docs as a `DOC.md` beside each `impl
//! LintImpl` (reached through [`LintImpl::DOC`]); the three pipeline meta
//! lints — `config`, `stale-expect`, `unknown-lint` — have no lint dir, so
//! their docs live here as sibling `.md` files. [`lint_doc`] is the one place
//! that unifies both, exhaustively over `LintId`, so a new variant can't land
//! without a doc.
//!
//! The docs surface two ways: `workspace-lint explain <lint>` prints one to
//! stdout, and clap attaches each to its `check <lint> --help` long help. Both
//! render in a terminal, so the `DOC.md` authoring rules (enforced by the
//! tests below) are: first line `# <short-name>`; only headings from the
//! closed schema set (`## What it checks` / `## Configuration` / `## Silencing`
//! required, `## Fix behavior` / `## Examples` / `## Known limits` optional);
//! ATX headings and fenced code only — no pipe tables, no HTML; and prose
//! hard-wrapped so no line outside a code fence exceeds 80 columns (clap ships
//! without `wrap_help`, so unwrapped lines would overflow narrow terminals).

use wl_lint_api::{LintId, LintImpl};
use wl_lints::{
    architecture::Architecture, centralized_deps::CentralizedDeps,
    cli_crate_version::CliCrateVersion, crate_size::CrateSize, duplicate_code::DuplicateCode,
    feature_drift::FeatureDrift, file_size::FileSize, freshness::Freshness,
    module_tree::ModuleTree, stale_git_index::StaleGitIndex, unused_deps::UnusedDeps,
    unused_pub::UnusedPub,
};

/// The full documentation for `id`. Exhaustive by design: adding a [`LintId`]
/// variant without wiring its doc is a compile error, mirroring `LintId::id`.
/// The 12 real lints resolve to their `DOC.md` const; the 3 meta lints to a
/// sibling `.md` bundled here.
pub(crate) fn lint_doc(id: LintId) -> &'static str {
    match id {
        LintId::Architecture => Architecture::DOC,
        LintId::CentralizedDeps => CentralizedDeps::DOC,
        LintId::CliCrateVersion => CliCrateVersion::DOC,
        LintId::Config => include_str!("config.md"),
        LintId::CrateSize => CrateSize::DOC,
        LintId::DuplicateCode => DuplicateCode::DOC,
        LintId::FeatureDrift => FeatureDrift::DOC,
        LintId::FileSize => FileSize::DOC,
        LintId::Freshness => Freshness::DOC,
        LintId::ModuleTree => ModuleTree::DOC,
        LintId::StaleExpect => include_str!("stale-expect.md"),
        LintId::StaleGitIndex => StaleGitIndex::DOC,
        LintId::UnknownLint => include_str!("unknown-lint.md"),
        LintId::UnusedDeps => UnusedDeps::DOC,
        LintId::UnusedPub => UnusedPub::DOC,
    }
}

/// Resolve a user-typed lint name — short (`unused-pub`) or fully qualified
/// (`workspace-lint::unused-pub`) — to its [`LintId`]. On a miss, the `Err`
/// carries the closest known short name for a "did you mean …?" hint (or
/// `None` when nothing is close enough). Pure, so it is unit-tested directly
/// without spawning the binary.
pub(crate) fn resolve(name: &str) -> Result<LintId, Option<&'static str>> {
    let short = name.strip_prefix("workspace-lint::").unwrap_or(name);
    LintId::from_short(short).ok_or_else(|| {
        let known: Vec<&str> = LintId::ALL.iter().map(|id| id.short()).collect();
        crate::suggest::closest(short, &known)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    const REQUIRED_HEADINGS: &[&str] = &["## What it checks", "## Configuration", "## Silencing"];
    const OPTIONAL_HEADINGS: &[&str] = &["## Fix behavior", "## Examples", "## Known limits"];
    /// The pipeline meta lints, whose docs live beside this module rather than
    /// in a lint dir (see [`lint_doc`]).
    const META: &[LintId] = &[LintId::Config, LintId::StaleExpect, LintId::UnknownLint];

    /// The repo-relative file a lint's doc lives in — for actionable failure
    /// messages and the README-link check.
    fn doc_path(id: LintId) -> String {
        if META.contains(&id) {
            format!("crates/workspace-lint/src/docs/{}.md", id.short())
        } else {
            format!(
                "crates/wl-lints/src/{}/DOC.md",
                id.short().replace('-', "_")
            )
        }
    }

    /// Every lint's doc obeys the terminal-readable schema (see the module
    /// docs). A crossed [`lint_doc`] match arm also trips the title check here.
    #[test]
    fn every_lint_doc_matches_the_schema() {
        for &id in LintId::ALL {
            let doc = lint_doc(id);
            let path = doc_path(id);
            assert!(!doc.is_empty(), "{path} is empty");
            assert_eq!(
                doc.lines().next(),
                Some(format!("# {}", id.short()).as_str()),
                "{path} must start with `# {}` (the title is the lint's short name)",
                id.short()
            );

            let mut in_fence = false;
            let mut headings: Vec<&str> = Vec::new();
            for (i, line) in doc.lines().enumerate() {
                let trimmed = line.trim_start();
                if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
                    in_fence = !in_fence;
                    continue;
                }
                if in_fence {
                    continue;
                }
                assert!(
                    line.chars().count() <= 80,
                    "{path}:{} is {} columns — hard-wrap prose at 80 (clap doesn't wrap):\n  {line}",
                    i + 1,
                    line.chars().count()
                );
                assert!(
                    !trimmed.starts_with('|'),
                    "{path}:{} is a pipe table — use a bullet list (tables don't render in a terminal)",
                    i + 1
                );
                if line.starts_with("## ") {
                    headings.push(line);
                }
            }
            assert!(!in_fence, "{path} has an unclosed code fence");

            for req in REQUIRED_HEADINGS {
                assert!(
                    headings.contains(req),
                    "{path} is missing required heading `{req}`"
                );
            }
            for h in &headings {
                assert!(
                    REQUIRED_HEADINGS.contains(h) || OPTIONAL_HEADINGS.contains(h),
                    "{path} has non-schema heading `{h}` (allowed: {REQUIRED_HEADINGS:?} + {OPTIONAL_HEADINGS:?})"
                );
            }
        }
    }

    /// Each real lint's `lint_doc` arm resolves to *its own* `DOC` const — an
    /// explicit guard against a crossed match arm.
    #[test]
    fn lint_doc_wires_each_real_lint_to_its_own_const() {
        assert_eq!(lint_doc(LintId::Architecture), Architecture::DOC);
        assert_eq!(lint_doc(LintId::CentralizedDeps), CentralizedDeps::DOC);
        assert_eq!(lint_doc(LintId::CliCrateVersion), CliCrateVersion::DOC);
        assert_eq!(lint_doc(LintId::CrateSize), CrateSize::DOC);
        assert_eq!(lint_doc(LintId::DuplicateCode), DuplicateCode::DOC);
        assert_eq!(lint_doc(LintId::FeatureDrift), FeatureDrift::DOC);
        assert_eq!(lint_doc(LintId::FileSize), FileSize::DOC);
        assert_eq!(lint_doc(LintId::Freshness), Freshness::DOC);
        assert_eq!(lint_doc(LintId::ModuleTree), ModuleTree::DOC);
        assert_eq!(lint_doc(LintId::StaleGitIndex), StaleGitIndex::DOC);
        assert_eq!(lint_doc(LintId::UnusedDeps), UnusedDeps::DOC);
        assert_eq!(lint_doc(LintId::UnusedPub), UnusedPub::DOC);
    }

    /// Every `check <lint>` subcommand carries its lint's doc as long help, so
    /// `check <lint> --help` and `explain <lint>` show the same text. Also
    /// guards against a new `CheckRule` variant landing without the attribute.
    #[test]
    fn check_subcommand_help_carries_the_lint_doc() {
        let cmd = crate::cli::Cli::command();
        let check = cmd
            .find_subcommand("check")
            .expect("the check subcommand exists");
        for sub in check.get_subcommands() {
            let name = sub.get_name();
            let id = LintId::from_short(name)
                .unwrap_or_else(|| panic!("check subcommand `{name}` is not a lint short name"));
            let help = sub
                .get_after_long_help()
                .unwrap_or_else(|| {
                    panic!("check {name} has no after_long_help (missing attribute)")
                })
                .to_string();
            assert_eq!(
                help.trim_end(),
                lint_doc(id).trim_end(),
                "check {name}'s long help must be its own DOC.md"
            );
        }
    }

    /// The README links every lint's doc file, so the docs stay discoverable
    /// and a renamed file can't silently orphan the link.
    #[test]
    fn readme_links_every_lint_doc() {
        let readme =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../README.md"))
                .expect("README.md is readable");
        for &id in LintId::ALL {
            let path = doc_path(id);
            assert!(readme.contains(&path), "README.md must link to `{path}`");
        }
    }

    #[test]
    fn resolve_accepts_short_and_fully_qualified_names() {
        assert_eq!(resolve("unused-pub"), Ok(LintId::UnusedPub));
        assert_eq!(resolve("workspace-lint::unused-pub"), Ok(LintId::UnusedPub));
    }

    #[test]
    fn resolve_suggests_the_closest_lint_on_a_typo() {
        assert_eq!(resolve("unused-dep"), Err(Some("unused-deps")));
    }

    #[test]
    fn resolve_gives_no_suggestion_when_nothing_is_close() {
        assert_eq!(resolve("zzzzzzzzzz"), Err(None));
    }
}
