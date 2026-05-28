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
//!   are not tracked.
//! - Pub items inside `impl` blocks are not yet enumerated as separate items.
//! - `#[derive(Serialize, Deserialize, ...)]`-suppressed cases need explicit
//!   `allowlist` globs or `#[derive(...)]`-aware suppression in a follow-up.

use globset::{Glob, GlobSet, GlobSetBuilder};
use std::collections::HashSet;
use syn_workspace::{Item, ItemKind, Module, ResolvedPath, Visibility, Workspace};

use crate::diagnostic::Diagnostic;
use crate::diagnostic::builder::at_line;
use crate::lints::{Lint, LintContext, LintId, Requirements};

pub mod config;
#[cfg(test)]
mod tests;

pub(crate) use config::UnusedPubConfig;

pub(crate) struct UnusedPub {
    config: UnusedPubConfig,
}

impl UnusedPub {
    pub fn new(config: UnusedPubConfig) -> Self {
        Self { config }
    }

    pub fn from_cli(
        exclude_crates: Vec<String>,
        allowlist: Vec<String>,
        kinds: Vec<String>,
        exclude_paths: Vec<String>,
        suppress_intra_crate: bool,
    ) -> Self {
        Self::new(UnusedPubConfig {
            exclude_crates,
            allowlist,
            kinds,
            exclude_paths,
            suppress_intra_crate,
            // `--fix` deletion is opt-in via config only — there's no CLI
            // override because deletion is irreversible-without-git and we
            // want the choice to live in the project's config file (not a
            // forgotten shell history line).
            auto_delete: false,
        })
    }
}

impl Lint for UnusedPub {
    fn id(&self) -> LintId {
        LintId::UnusedPub
    }

    fn requirements(&self) -> Requirements {
        Requirements {
            needs_workspace: true,
        }
    }

    fn check(&self, cx: &LintContext<'_>) -> Vec<Diagnostic> {
        let workspace = cx
            .workspace
            .expect("unused-pub lint requires Workspace (Requirements::needs_workspace)");
        check(&self.config, workspace)
    }
}

pub(crate) fn check(config: &UnusedPubConfig, workspace: &Workspace) -> Vec<Diagnostic> {
    let kind_filter = parse_kind_filter(&config.kinds);
    let allowlist = build_glob_set(&config.allowlist, "allowlist");
    let exclude_paths = build_glob_set(&config.exclude_paths, "exclude-paths");

    let mut diagnostics = Vec::new();

    // `pub` items in tests / build scripts / benches aren't part of the
    // cross-crate API surface, so we only scan each member's primary unit
    // (lib / proc-macro / main bin).
    for (krate, target) in workspace.primary_units() {
        let crate_code = krate.code_name();
        if config
            .exclude_crates
            .iter()
            .any(|c| c == &krate.name || c == &crate_code)
        {
            continue;
        }
        let macro_refs = workspace.macro_implicit_refs_for(krate);
        let ctx = CheckCtx {
            workspace,
            crate_code: &crate_code,
            macro_refs: &macro_refs,
            kind_filter: kind_filter.as_ref(),
            allowlist: allowlist.as_ref(),
            exclude_paths: exclude_paths.as_ref(),
            suppress_intra_crate: config.suppress_intra_crate,
            auto_delete: config.auto_delete,
        };
        for (module, item) in target.root.walk_items() {
            if let Some(d) = check_item(module, item, &ctx) {
                diagnostics.push(d);
            }
        }
    }

    diagnostics
}

struct CheckCtx<'a> {
    workspace: &'a Workspace,
    crate_code: &'a str,
    macro_refs: &'a HashSet<ResolvedPath>,
    kind_filter: Option<&'a HashSet<ItemKind>>,
    allowlist: Option<&'a GlobSet>,
    exclude_paths: Option<&'a GlobSet>,
    suppress_intra_crate: bool,
    auto_delete: bool,
}

fn check_item(module: &Module, item: &Item, ctx: &CheckCtx<'_>) -> Option<Diagnostic> {
    if !item.kind.is_definition() {
        return None;
    }
    if item.visibility != Visibility::Public {
        return None;
    }
    if item.name == "main" && module.canonical.segments().len() == 1 {
        return None;
    }
    if let Some(kf) = ctx.kind_filter
        && !kf.contains(&item.kind)
    {
        return None;
    }
    if let Some(al) = ctx.allowlist
        && al.is_match(item.canonical.display())
    {
        return None;
    }
    if let Some(ex) = ctx.exclude_paths
        && let Some(span) = &item.source
        && ex.is_match(span.file.to_string_lossy().as_ref())
    {
        return None;
    }
    if ctx.macro_refs.contains(&item.canonical) {
        return None;
    }
    // Skip items reachable via a `pub use` chain — those are load-bearing
    // for the re-export's compilation and form part of the containing
    // crate's public API surface (the workspace resolver may see no
    // in-workspace consumer for the re-exported name, but external
    // consumers of a library crate do). Narrowing them to `pub(crate)`
    // produces E0364 / E0365 at the re-export site.
    if ctx.workspace.re_exports().is_target(&item.canonical) {
        return None;
    }
    // Skip items in a published library's public API surface — same logic
    // as the `is_target` gate above, but for items reached via ordinary
    // `pub mod` chains rather than `pub use` re-exports.
    if ctx.workspace.is_externally_reachable(&item.canonical) {
        return None;
    }

    let referring = ctx.workspace.referring_crates(&item.canonical);
    let used_cross_crate = referring
        .map(|set| set.iter().any(|c| c != ctx.crate_code))
        .unwrap_or(false);
    if used_cross_crate {
        return None;
    }
    let used_same_crate = referring
        .map(|set| set.contains(ctx.crate_code))
        .unwrap_or(false);

    if used_same_crate && ctx.suppress_intra_crate {
        return None;
    }

    let span = item.source.as_ref()?;

    let kind_str = item.kind;
    let crate_code = ctx.crate_code;
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

    let mut builder = at_line(LintId::UnusedPub.id(), message, span.file.clone(), span.line)
        .help(suggestion)
        .note(
            "#[cfg]-gated items, proc-macro usage, trait-method dispatch, and re-exports may cause false positives",
        );
    // Structural fix policy:
    //  - "only used inside the crate" → always pub → pub(crate).
    //  - "appears unused" + auto_delete on + file is git-tracked-clean
    //    → delete the item.
    //  - "appears unused" + auto_delete on + file is dirty/untracked
    //    → emit deletion suggestion as MaybeIncorrect (--fix skips
    //    those) with an extra note explaining why.
    //  - "appears unused" + auto_delete off → pub → pub(crate).
    let want_delete = !used_same_crate && ctx.auto_delete;
    if want_delete {
        match delete_suggestion(span) {
            DeleteOutcome::Apply(s) => builder = builder.suggestion(s),
            DeleteOutcome::Skip(s, reason) => {
                builder = builder.suggestion(s).note(reason);
            }
            DeleteOutcome::Unavailable => {
                if let Some(s) = crate::lints::visibility::build_tighten_suggestion(item) {
                    builder = builder.suggestion(s);
                }
            }
        }
    } else if let Some(s) = crate::lints::visibility::build_tighten_suggestion(item) {
        builder = builder.suggestion(s);
    }
    Some(builder.build())
}

enum DeleteOutcome {
    /// Git-tracked-clean: emit a MachineApplicable deletion suggestion.
    Apply(crate::diagnostic::Suggestion),
    /// Tracked-but-dirty or untracked: emit MaybeIncorrect so `--fix`
    /// passes over it, plus a reason note for the user.
    Skip(crate::diagnostic::Suggestion, String),
    /// Span has no byte range, file can't be read, etc. Fall back to the
    /// visibility-narrowing path.
    Unavailable,
}

fn delete_suggestion(span: &syn_workspace::SourceSpan) -> DeleteOutcome {
    let Some(range) = &span.byte_range else {
        return DeleteOutcome::Unavailable;
    };
    let Ok(source) = fs_err::read_to_string(&span.file) else {
        return DeleteOutcome::Unavailable;
    };
    let start = range.start as usize;
    let mut end = (range.end as usize).min(source.len());
    if start >= end {
        return DeleteOutcome::Unavailable;
    }
    if end < source.len() && source.as_bytes()[end] == b'\n' {
        end += 1;
    }
    let applicability = if is_file_clean_in_git(&span.file) {
        crate::diagnostic::Applicability::MachineApplicable
    } else {
        crate::diagnostic::Applicability::MaybeIncorrect
    };
    let suggestion = crate::diagnostic::Suggestion {
        span: crate::diagnostic::Span {
            file: span.file.clone(),
            line_start: span.line,
            line_end: span.line,
            col_start: 1,
            col_end: 1,
            byte_start: start as u32,
            byte_end: end as u32,
        },
        message: "delete the unused item".into(),
        replacement: String::new(),
        applicability,
    };
    if applicability == crate::diagnostic::Applicability::MachineApplicable {
        DeleteOutcome::Apply(suggestion)
    } else {
        DeleteOutcome::Skip(
            suggestion,
            format!(
                "file `{}` is untracked or has uncommitted changes; `--fix` will not auto-delete (commit first or use `git stash`)",
                span.file.display()
            ),
        )
    }
}

/// `true` iff `path` is tracked by git AND has no uncommitted changes.
/// Returns `false` if we can't determine the state — git missing, not a
/// repo, path outside the repo, command failure. The safer default is to
/// downgrade the suggestion's applicability so `--fix` skips it.
fn is_file_clean_in_git(path: &std::path::Path) -> bool {
    use std::process::Command;
    let ls = Command::new("git")
        .args(["ls-files", "--error-unmatch", "--"])
        .arg(path)
        .output();
    let Ok(out) = ls else { return false };
    if !out.status.success() {
        return false;
    }
    let st = Command::new("git")
        .args(["status", "--porcelain", "--"])
        .arg(path)
        .output();
    let Ok(out) = st else { return false };
    if !out.status.success() {
        return false;
    }
    out.stdout.is_empty()
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
