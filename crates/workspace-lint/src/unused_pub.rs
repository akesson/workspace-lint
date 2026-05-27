//! Resolver-backed unused-pub check.
//!
//! Flags `pub` items that have no cross-crate references. Items used only
//! intra-crate get a "tighten to `pub(crate)`" suggestion; items with no
//! references at all get a "remove" suggestion.
//!
//! Built on [`syn_workspace::Workspace`] — no SCIP, no `rust-analyzer`
//! subprocess. Known limitations carried over from the resolver model
//! (documented in tests/cases/visibility/known_false_positives/ for the
//! sibling visibility lint):
//!
//! - Trait methods dispatched through `dyn Trait` or generic method calls
//!   are not tracked — the resolver doesn't do type inference.
//! - Pub items inside `impl` blocks (`pub fn` on inherent impls, `pub`
//!   associated consts/types) are not yet enumerated as separate items.
//!   The resolver currently models module-level items only.
//! - `#[derive(Serialize, Deserialize, ...)]`-suppressed cases that the
//!   SCIP backend handled need to be papered over with explicit `allowlist`
//!   globs or `#[derive(...)]`-aware suppression in a follow-up.

use crate::config::UnusedPubConfig;
use crate::diagnostic::Diagnostic;
use crate::diagnostic::builder::at_line;
use globset::{Glob, GlobSet, GlobSetBuilder};
use std::collections::{HashMap, HashSet};
use syn_workspace::{Item, ItemKind, Module, ResolvedPath, Visibility, Workspace};

pub const LINT: &str = "workspace-lint::unused-pub";

pub fn check(config: &UnusedPubConfig, workspace: &Workspace) -> Vec<Diagnostic> {
    if config.effective_on_ci_only() && std::env::var("CI").is_err() {
        return Vec::new();
    }

    // Per-canonical-path: set of crates that reference it (excluding the
    // defining crate). Empty entry means "referenced only intra-crate"; no
    // entry at all means "not referenced anywhere in the workspace".
    let references_by_path = build_reference_index(workspace);
    let macro_refs = workspace.macro_implicit_refs();
    let kind_filter = parse_kind_filter(&config.kinds);
    let allowlist = build_glob_set(&config.allowlist, "allowlist");
    let exclude_paths = build_glob_set(&config.exclude_paths, "exclude-paths");

    let mut diagnostics = Vec::new();

    for krate in workspace.members() {
        let crate_code = krate.code_name();
        if config
            .exclude_crates
            .iter()
            .any(|c| c == &krate.name || c == &crate_code)
        {
            continue;
        }
        collect_findings(
            &krate.root,
            &crate_code,
            &references_by_path,
            macro_refs,
            kind_filter.as_ref(),
            allowlist.as_ref(),
            exclude_paths.as_ref(),
            config.suppress_intra_crate,
            &mut diagnostics,
        );
    }

    diagnostics
}

/// Build a `canonical_path → set<referring_crate>` index from the
/// workspace's per-crate reference sets. The defining crate is excluded
/// from each entry, so checking "referenced from another crate" reduces to
/// "entry exists and is non-empty".
fn build_reference_index(workspace: &Workspace) -> HashMap<ResolvedPath, HashSet<String>> {
    let mut out: HashMap<ResolvedPath, HashSet<String>> = HashMap::new();
    for (referring_crate, path) in workspace.iter_references() {
        // Follow re-export chains so an item used via `pub use` counts as a
        // reference to its canonical definition site, not the re-export.
        let canonical = workspace.resolve_canonical(path);
        let entry = out.entry(canonical).or_default();
        entry.insert(referring_crate.to_string());
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn collect_findings(
    module: &Module,
    crate_code: &str,
    refs_by_path: &HashMap<ResolvedPath, HashSet<String>>,
    macro_refs: &HashSet<ResolvedPath>,
    kind_filter: Option<&HashSet<ItemKind>>,
    allowlist: Option<&GlobSet>,
    exclude_paths: Option<&GlobSet>,
    suppress_intra_crate: bool,
    out: &mut Vec<Diagnostic>,
) {
    for item in &module.items {
        if !checkable(item) {
            continue;
        }
        if item.visibility != Visibility::Public {
            continue;
        }
        // Crate-root `pub fn main()` is the bin entry point; cargo needs it
        // pub for entry-point resolution.
        if item.name == "main" && module.canonical.segments().len() == 1 {
            continue;
        }
        if let Some(kf) = kind_filter
            && !kf.contains(&item.kind)
        {
            continue;
        }
        if let Some(al) = allowlist
            && al.is_match(item.canonical.display())
        {
            continue;
        }
        if let Some(ex) = exclude_paths
            && let Some(span) = &item.source
            && ex.is_match(span.file.to_string_lossy().as_ref())
        {
            continue;
        }
        // Macro-body reachability is a workspace-wide suppression channel:
        // any item appearing in any `macro_rules!` body is potentially
        // reachable from any macro call site, so don't flag.
        if macro_refs.contains(&item.canonical) {
            continue;
        }

        let referring = refs_by_path.get(&item.canonical);
        let used_cross_crate = referring
            .map(|set| set.iter().any(|c| c != crate_code))
            .unwrap_or(false);
        if used_cross_crate {
            continue;
        }
        let used_same_crate = referring
            .map(|set| set.contains(crate_code))
            .unwrap_or(false);

        if used_same_crate && suppress_intra_crate {
            continue;
        }

        let Some(span) = &item.source else {
            continue;
        };

        let kind_str = format_kind(item.kind);
        let (message, suggestion) = if used_same_crate {
            (
                format!(
                    "pub {kind_str} `{}` in crate `{crate_code}` is only used inside the crate",
                    item.name
                ),
                "consider `pub(crate)` to tighten visibility",
            )
        } else {
            (
                format!(
                    "pub {kind_str} `{}` in crate `{crate_code}` appears unused — consider removing",
                    item.name
                ),
                "remove the item or its `pub` visibility",
            )
        };

        out.push(
            at_line(LINT, message, span.file.clone(), span.line)
                .help(suggestion)
                .note(
                    "#[cfg]-gated items, proc-macro usage, trait-method dispatch, and re-exports may cause false positives",
                )
                .build(),
        );
    }

    for sub in &module.submodules {
        collect_findings(
            sub,
            crate_code,
            refs_by_path,
            macro_refs,
            kind_filter,
            allowlist,
            exclude_paths,
            suppress_intra_crate,
            out,
        );
    }
}

fn checkable(item: &Item) -> bool {
    matches!(
        item.kind,
        ItemKind::Fn
            | ItemKind::Struct
            | ItemKind::Enum
            | ItemKind::Union
            | ItemKind::Trait
            | ItemKind::TypeAlias
            | ItemKind::Const
            | ItemKind::Static
            | ItemKind::Macro
    )
}

fn format_kind(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Fn => "fn",
        ItemKind::Struct => "struct",
        ItemKind::Enum => "enum",
        ItemKind::Union => "union",
        ItemKind::Trait => "trait",
        ItemKind::TypeAlias => "type",
        ItemKind::Const => "const",
        ItemKind::Static => "static",
        ItemKind::Module => "mod",
        ItemKind::Macro => "macro",
        ItemKind::Impl => "impl",
        ItemKind::Use => "use",
        ItemKind::ExternCrate => "extern crate",
    }
}

fn parse_kind_filter(kinds: &[String]) -> Option<HashSet<ItemKind>> {
    if kinds.is_empty() {
        return None;
    }
    let mut set = HashSet::new();
    for kind_str in kinds {
        match kind_str.to_lowercase().as_str() {
            "function" | "fn" => {
                set.insert(ItemKind::Fn);
            }
            "struct" => {
                set.insert(ItemKind::Struct);
            }
            "enum" => {
                set.insert(ItemKind::Enum);
            }
            "union" => {
                set.insert(ItemKind::Union);
            }
            "trait" => {
                set.insert(ItemKind::Trait);
            }
            "type" | "type_alias" => {
                set.insert(ItemKind::TypeAlias);
            }
            "const" | "constant" => {
                set.insert(ItemKind::Const);
            }
            "static" => {
                set.insert(ItemKind::Static);
            }
            "module" | "mod" => {
                set.insert(ItemKind::Module);
            }
            "macro" => {
                set.insert(ItemKind::Macro);
            }
            other => {
                eprintln!("warning: unknown unused-pub kind filter `{other}`, ignoring");
            }
        }
    }
    Some(set)
}

fn build_glob_set(patterns: &[String], label: &str) -> Option<GlobSet> {
    if patterns.is_empty() {
        return None;
    }
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern).unwrap_or_else(|e| {
            eprintln!("warning: invalid {label} glob `{pattern}`: {e}");
            std::process::exit(1);
        }));
    }
    Some(builder.build().unwrap_or_else(|e| {
        eprintln!("failed to build {label} filter: {e}");
        std::process::exit(1);
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_filter_parses_aliases() {
        let filter = parse_kind_filter(&["fn".into(), "function".into(), "type".into()]).unwrap();
        assert!(filter.contains(&ItemKind::Fn));
        assert!(filter.contains(&ItemKind::TypeAlias));
    }

    #[test]
    fn kind_filter_ignores_unknown_kinds() {
        let filter = parse_kind_filter(&["banana".into(), "fn".into()]).unwrap();
        assert_eq!(filter.len(), 1);
        assert!(filter.contains(&ItemKind::Fn));
    }

    #[test]
    fn kind_filter_empty_returns_none() {
        assert!(parse_kind_filter(&[]).is_none());
    }

    #[test]
    fn glob_set_returns_none_for_empty() {
        assert!(build_glob_set(&[], "test").is_none());
    }

    #[test]
    fn format_kind_covers_all_variants() {
        // Don't crash on any ItemKind — important since the resolver may
        // extend this enum in the future and we'd want a quick visual check.
        for k in [
            ItemKind::Fn,
            ItemKind::Struct,
            ItemKind::Enum,
            ItemKind::Union,
            ItemKind::Trait,
            ItemKind::TypeAlias,
            ItemKind::Const,
            ItemKind::Static,
            ItemKind::Module,
            ItemKind::Macro,
            ItemKind::Impl,
            ItemKind::Use,
            ItemKind::ExternCrate,
        ] {
            let _ = format_kind(k);
        }
    }
}
