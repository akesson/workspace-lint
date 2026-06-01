//! Resolver-backed unused-pub check.
//!
//! Flags `pub` items that have no cross-crate references. Items used only
//! intra-crate get a "tighten to `pub(crate)`" suggestion; items with no
//! references at all get a "remove" suggestion.
//!
//! ## Publish-awareness
//!
//! The resolver can't see consumers outside the workspace, so a crate's
//! library-public API is exempt as "external API surface" *only* when the crate
//! declares it's published — `publish = true` or a registry list (see
//! [`Workspace::resolved_publish`](syn_workspace::Workspace::resolved_publish)).
//! A crate with `publish = false`, or — by default — no `publish` field, is
//! treated as workspace-internal: its `pub` items go through the cross-crate
//! check, so over-exposed internal APIs get flagged. `assume-all-public` opts
//! out (treat every crate as external). When an internal crate accumulates
//! `publish-hint-threshold` findings, a crate-level hint nudges `publish = true`.
//!
//! ## Known limitation (definition-site self-reference)
//!
//! The resolver records a bare ident that names a same-module sibling as a
//! reference (so `Foo` used by bare name counts) — but it also scans an item's
//! *own* declaration tokens, so a definition's name counts as a self-reference
//! to itself. Consequence: in practice a never-used `pub fn foo` in a single
//! crate is classified `IntraCrate` ("used only inside the crate") rather than
//! `Unused`. The `pub(crate)` suggestion is still correct, but the "remove"
//! message rarely fires for named items, and under `suppress-intra-crate` such
//! items are silenced. Tracked as a separate resolver fix.
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

use globset::{GlobSet, GlobSetBuilder};
use std::collections::{HashMap, HashSet};
use syn_workspace::{Crate, Item, ItemKind, Module, Publish, ResolvedPath, Visibility, Workspace};

use crate::config::GlobPattern;
use crate::diagnostic::Diagnostic;
use crate::diagnostic::builder::{at_crate, at_line};

/// Number of unused-pub findings an internal crate must accumulate before we
/// emit the one-time `publish = true` hint. Used when the config leaves
/// `publish-hint-threshold` unset.
const DEFAULT_PUBLISH_HINT_THRESHOLD: usize = 3;
use crate::lints::{Lint, LintContext, LintId, Requirements};

pub mod config;
#[cfg(test)]
mod tests;

pub(crate) use config::{KindFilter, UnusedPubConfig};

pub(crate) struct UnusedPub {
    /// Workspace-wide params, used for any crate without a per-crate section.
    global: UnusedPubConfig,
    /// Per-crate params (keyed by Cargo package name), each *wholesale*
    /// replacing the global config for that crate. Empty for CLI single-check
    /// runs, which have no `[crates.*]` tier.
    per_crate: HashMap<String, UnusedPubConfig>,
}

impl UnusedPub {
    pub fn new(global: UnusedPubConfig, per_crate: HashMap<String, UnusedPubConfig>) -> Self {
        Self { global, per_crate }
    }

    pub fn from_cli(
        exclude_crates: Vec<String>,
        allowlist: Vec<String>,
        kinds: Vec<KindFilter>,
        exclude_paths: Vec<String>,
        suppress_intra_crate: bool,
    ) -> Self {
        Self::new(
            UnusedPubConfig {
                exclude_crates,
                allowlist: allowlist.iter().map(|p| GlobPattern::from_cli(p)).collect(),
                kinds,
                exclude_paths: exclude_paths
                    .iter()
                    .map(|p| GlobPattern::from_cli(p))
                    .collect(),
                suppress_intra_crate,
                // `--fix` deletion is opt-in via config only — there's no CLI
                // override because deletion is irreversible-without-git and we
                // want the choice to live in the project's config file (not a
                // forgotten shell history line).
                auto_delete: false,
                // Publish-awareness is config-only (no CLI flags): both live in the
                // project's config file. CLI single-lint runs keep the defaults.
                assume_all_public: false,
                publish_hint_threshold: None,
            },
            HashMap::new(),
        )
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
        check(&self.global, &self.per_crate, workspace)
    }
}

pub(crate) fn check(
    global: &UnusedPubConfig,
    per_crate: &HashMap<String, UnusedPubConfig>,
    workspace: &Workspace,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    // `pub` items in tests / build scripts / benches aren't part of the
    // cross-crate API surface, so we only scan each member's primary unit
    // (lib / proc-macro / main bin).
    for (krate, target) in workspace.primary_units() {
        // A per-crate `[crates.<name>.unused-pub]` wholesale-replaces the
        // global params for this crate; the glob sets / kind filter are built
        // from the resolved config, so they're computed per crate rather than
        // once up front.
        let config = per_crate.get(&krate.name).unwrap_or(global);
        let kind_filter: Option<HashSet<ItemKind>> = (!config.kinds.is_empty())
            .then(|| config.kinds.iter().map(|k| k.to_item_kind()).collect());
        let allowlist = build_glob_set(&config.allowlist);
        let exclude_paths = build_glob_set(&config.exclude_paths);

        let crate_code = krate.code_name();
        if config
            .exclude_crates
            .iter()
            .any(|c| c == &krate.name || c == &crate_code)
        {
            continue;
        }
        let macro_refs = workspace.macro_implicit_refs_for(krate);
        // A crate's library-public items are exempt as "external API surface"
        // only when the crate actually has out-of-workspace consumers — i.e. it
        // declares `publish = true` (or a registry list) — or the user opted
        // every crate in via `assume-all-public`. Otherwise the crate is treated
        // as workspace-internal: its `pub` items go through the normal
        // cross-crate-usage check, so an item unused across the workspace is
        // flagged. (`cargo metadata` can't see an explicit `publish = true`, so
        // this reads the manifest via `resolved_publish`.)
        let exempt_external_api = config.assume_all_public || crate_is_public(workspace, krate);
        let ctx = CheckCtx {
            workspace,
            crate_code: &crate_code,
            macro_refs: &macro_refs,
            kind_filter: kind_filter.as_ref(),
            allowlist: allowlist.as_ref(),
            exclude_paths: exclude_paths.as_ref(),
            suppress_intra_crate: config.suppress_intra_crate,
            auto_delete: config.auto_delete,
            exempt_external_api,
        };
        let mut crate_diags = Vec::new();
        for (module, item) in target.root.walk_items() {
            if let Some(d) = check_item(module, item, &ctx) {
                crate_diags.push(d);
            }
        }
        // When a *workspace-internal* crate (treated as such only because it
        // didn't declare `publish = true`) accumulates several findings, the
        // likely cause is that it really is published — nudge the author toward
        // the one-line fix. Self-resolving: adding `publish = true` exempts the
        // items, so the findings and this hint both disappear.
        let threshold = config
            .publish_hint_threshold
            .unwrap_or(DEFAULT_PUBLISH_HINT_THRESHOLD);
        if !exempt_external_api && threshold > 0 && crate_diags.len() >= threshold {
            diagnostics.push(publish_hint(krate, crate_diags.len()));
        }
        diagnostics.extend(crate_diags);
    }

    diagnostics
}

/// Whether `krate` declares a real external API (`publish = true` or a registry
/// list), resolving `publish.workspace = true` against the workspace root. An
/// absent or `false` `publish` field is treated as workspace-internal — the
/// opinionated default that lets `unused-pub` flag over-exposed internal APIs.
fn crate_is_public(workspace: &Workspace, krate: &Crate) -> bool {
    matches!(
        workspace.resolved_publish(krate),
        Publish::ExplicitTrue | Publish::Registries(_)
    )
}

/// One crate-level hint suggesting `publish = true` for an internal crate that
/// produced `count` findings. Anchored at the crate so the silence directive
/// (and the human "crate" grain) point at the right place.
fn publish_hint(krate: &Crate, count: usize) -> Diagnostic {
    at_crate(
        LintId::UnusedPub.id(),
        format!(
            "crate `{}` has {count} public items unused within the workspace",
            krate.code_name()
        ),
        krate.manifest_dir.clone(),
    )
    .help(format!(
        "if `{}` is published outside this workspace, set `publish = true` in its Cargo.toml \
         to treat its public API as external (these findings become exempt)",
        krate.name
    ))
    .note(
        "workspace-lint treats a crate as workspace-internal unless it declares `publish = true` \
         (or a registry); see the unused-pub docs",
    )
    .build()
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
    /// Whether library-public items in this crate are exempt as external API
    /// surface (the crate is published, or `assume-all-public` is set). When
    /// `false`, the crate is workspace-internal and its `pub` items go through
    /// the cross-crate-usage check.
    exempt_external_api: bool,
}

/// Cross-crate usage classification, computed once per candidate item.
/// Drives both the per-item skip decision (Cross or intra-crate-suppressed
/// are both no-ops) and the diagnostic message shape (intra-crate use ⇒
/// "tighten", unused entirely ⇒ "remove").
enum Usage {
    /// Referenced from at least one other workspace crate — leave alone.
    CrossCrate,
    /// Only referenced inside the owning crate; suggest `pub(crate)`.
    IntraCrate,
    /// No references anywhere the resolver can see; suggest removing or
    /// (with `auto_delete = true`) delete outright.
    Unused,
}

fn check_item(module: &Module, item: &Item, ctx: &CheckCtx<'_>) -> Option<Diagnostic> {
    if item_skipped_by_filters(module, item, ctx) {
        return None;
    }
    let usage = classify_usage(item, ctx);
    if matches!(usage, Usage::CrossCrate) {
        return None;
    }
    if matches!(usage, Usage::IntraCrate) && ctx.suppress_intra_crate {
        return None;
    }
    let span = item.source.as_ref()?;
    Some(build_diagnostic(item, ctx, span, &usage))
}

/// Pure filter cascade: every reason to bail before doing the expensive
/// reference-set lookup goes here. Kept side-effect-free so the
/// fast-path early-out logic doesn't tangle with the diagnostic
/// composition in `check_item`, and so the CC of `check_item` itself
/// stays manageable (CRAP gate fix).
fn item_skipped_by_filters(module: &Module, item: &Item, ctx: &CheckCtx<'_>) -> bool {
    if !item.kind.is_definition() {
        return true;
    }
    if item.visibility != Visibility::Public {
        return true;
    }
    if item.name == "main" && module.canonical.segments().len() == 1 {
        return true;
    }
    if let Some(kf) = ctx.kind_filter
        && !kf.contains(&item.kind)
    {
        return true;
    }
    if let Some(al) = ctx.allowlist
        && al.is_match(item.canonical.display())
    {
        return true;
    }
    if let Some(ex) = ctx.exclude_paths
        && let Some(span) = &item.source
        && ex.is_match(span.file.to_string_lossy().as_ref())
    {
        return true;
    }
    if ctx.macro_refs.contains(&item.canonical) {
        return true;
    }
    // A re-export target is part of the crate's API regardless of publish
    // status — narrowing it would break the `pub use` (E0364 / E0365).
    if ctx.workspace.re_exports().is_target(&item.canonical) {
        return true;
    }
    // Library-public reachability only exempts the item when the crate is
    // treated as having external (out-of-workspace) consumers — see
    // `exempt_external_api`. An internal crate's reachable `pub` items fall
    // through to the cross-crate-usage check below.
    if ctx.exempt_external_api && ctx.workspace.is_externally_reachable(&item.canonical) {
        return true;
    }
    false
}

fn classify_usage(item: &Item, ctx: &CheckCtx<'_>) -> Usage {
    let referring = ctx.workspace.referring_crates(&item.canonical);
    let used_cross = referring
        .map(|set| set.iter().any(|c| c != ctx.crate_code))
        .unwrap_or(false);
    if used_cross {
        return Usage::CrossCrate;
    }
    let used_same = referring
        .map(|set| set.contains(ctx.crate_code))
        .unwrap_or(false);
    if used_same {
        Usage::IntraCrate
    } else {
        Usage::Unused
    }
}

fn build_diagnostic(
    item: &Item,
    ctx: &CheckCtx<'_>,
    span: &syn_workspace::SourceSpan,
    usage: &Usage,
) -> Diagnostic {
    let kind_str = item.kind;
    let crate_code = ctx.crate_code;
    let (message, suggestion) = match usage {
        Usage::IntraCrate => (
            format!(
                "pub {kind_str} `{}` in crate `{crate_code}` is only used inside the crate",
                item.name
            ),
            "consider `pub(crate)` to tighten visibility",
        ),
        Usage::Unused | Usage::CrossCrate => (
            format!(
                "pub {kind_str} `{}` in crate `{crate_code}` appears unused — consider removing",
                item.name
            ),
            "remove the item or its `pub` visibility",
        ),
    };

    let builder = at_line(LintId::UnusedPub.id(), message, span.file.clone(), span.line)
        .help(suggestion)
        .note(
            "#[cfg]-gated items, proc-macro usage, trait-method dispatch, and re-exports may cause false positives",
        );
    apply_structural_fix(builder, item, ctx.auto_delete, span, usage).build()
}

/// Structural fix policy:
///  - `IntraCrate` → always `pub` → `pub(crate)`.
///  - `Unused` + `auto_delete = true` + git-tracked-clean → delete.
///  - `Unused` + `auto_delete = true` + dirty/untracked → emit deletion
///    as `MaybeIncorrect` (so `--fix` skips it) plus an explanatory note.
///  - `Unused` + `auto_delete = false` → narrow to `pub(crate)`.
///
/// `auto_delete` is passed as a `bool` rather than reaching into [`CheckCtx`]
/// so this is independently unit-testable.
fn apply_structural_fix(
    builder: crate::diagnostic::builder::DiagnosticBuilder,
    item: &Item,
    auto_delete: bool,
    span: &syn_workspace::SourceSpan,
    usage: &Usage,
) -> crate::diagnostic::builder::DiagnosticBuilder {
    if let Some((sugg, note)) = pick_deletion_fix(auto_delete, span, usage) {
        let with_sugg = builder.suggestion(sugg);
        return note.into_iter().fold(with_sugg, |b, reason| b.note(reason));
    }
    build_tighten_suggestion(item)
        .into_iter()
        .fold(builder, |b, s| b.suggestion(s))
}

/// Build a `MachineApplicable` suggestion that overwrites the item's `pub`
/// keyword with `pub(crate)`. The byte range comes from
/// [`Item::vis_byte_range`], which `syn-workspace` sets from the
/// `Visibility::Public` token's `proc-macro2` span — no source scanning, so
/// no risk of matching a `pub` token inside a doc comment or string literal.
/// Returns `None` for items without a captured visibility span.
fn build_tighten_suggestion(item: &Item) -> Option<crate::diagnostic::Suggestion> {
    let span = item.source.as_ref()?;
    let vis_range = item.vis_byte_range.clone()?;
    Some(crate::diagnostic::Suggestion {
        span: crate::diagnostic::Span {
            file: span.file.clone(),
            line_start: span.line,
            line_end: span.line,
            col_start: 1,
            col_end: 1,
            byte_start: vis_range.start,
            byte_end: vis_range.end,
        },
        message: "tighten to `pub(crate)`".into(),
        replacement: "pub(crate)".into(),
        applicability: crate::diagnostic::Applicability::MachineApplicable,
    })
}

/// Pick a deletion suggestion when the user asked for one (`auto_delete`)
/// and the item is genuinely unused. Returns `None` to mean "fall back to
/// the tightening suggestion" — either the usage class doesn't warrant
/// deletion, the user didn't opt in, or the file's byte range is
/// unavailable. The `Option<String>` second element carries the
/// "git-dirty file" caveat note when present.
fn pick_deletion_fix(
    auto_delete: bool,
    span: &syn_workspace::SourceSpan,
    usage: &Usage,
) -> Option<(crate::diagnostic::Suggestion, Option<String>)> {
    if !auto_delete || !matches!(usage, Usage::Unused) {
        return None;
    }
    match delete_suggestion(span) {
        DeleteOutcome::Apply(s) => Some((s, None)),
        DeleteOutcome::Skip(s, reason) => Some((s, Some(reason))),
        DeleteOutcome::Unavailable => None,
    }
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

fn build_glob_set(patterns: &[GlobPattern]) -> Option<GlobSet> {
    if patterns.is_empty() {
        return None;
    }
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(pattern.compiled().clone());
    }
    Some(builder.build().unwrap_or_else(|e| {
        eprintln!("failed to build glob filter: {e}");
        std::process::exit(1);
    }))
}
