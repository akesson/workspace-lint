//! The rustc-IR `unused-pub` backend (the default).
//!
//! Each syn query maps onto an engine primitive:
//!
//! - `Workspace::referring_crates` / `referenced_from_sibling_target` →
//!   [`PubUsage`]: the cfg-matrix-unioned cross/intra/unused split. Sibling
//!   targets (integration tests, benches) compile as their own crates, so
//!   their references classify cross-crate exactly like the syn rule — but
//!   only under a config that compiles them (`[engine] configs`).
//! - `Workspace::re_exports().is_target` → [`PubCandidate::reexport_target`].
//! - `Workspace::exposed_in_public_signature` →
//!   [`PubCandidate::signature_exposed`] (from lowered signatures, so the
//!   builder-macro exemptions syn special-cased come for free).
//! - `Workspace::is_externally_reachable` →
//!   [`PubCandidate::externally_reachable`] (pub-module-hop over `mod` facts).
//! - `Workspace::macro_implicit_refs_for` → **gone**: rustc sees macro
//!   expansions natively, so a macro-generated reference is an ordinary edge
//!   and the `expansion-uses` annotations are obsolete.
//! - `Workspace::resolved_publish` → [`FastModel::resolved_publish`].
//!
//! What the engine adds over syn: inherent-impl methods are candidates (the
//! `pub_method_in_impl_block` false negative flips to a true positive), and
//! trait-dispatch / `#[no_mangle]`-export reachability retires the
//! `ffi_no_mangle_export` false positive. Trait-impl items are judged for
//! reachability but never flagged — their visibility is trait-forced (no
//! tighten surface, deletion breaks the impl), mirroring rustc's own
//! `dead_code` scope.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use globset::{GlobSet, GlobSetBuilder};
use wl_engine::fast::{CrateInfo, FastModel, Publish};
use wl_engine::semantic::{Category, PubCandidate, PubUsage, SemanticModel};
use wl_engine::wl_ir;

use crate::LintId;
use crate::config::GlobPattern;
use wl_diagnostic::builder::{at_crate, at_line};
use wl_diagnostic::{Applicability, Diagnostic, PubVerdict};

use super::DEFAULT_PUBLISH_HINT_THRESHOLD;
use super::config::UnusedPubConfig;

/// One unused-pub finding paired with the two things the `--fix` cascade needs
/// beyond the rendered diagnostic: the candidate identity (the removed-set key)
/// and whether it is a genuinely-`Unused` item carrying a MachineApplicable
/// deletion — i.e. a removal seed that will actually be applied.
pub(crate) struct PubFinding {
    /// `PubCandidate::id`; `None` for the crate-level publish hint.
    pub id: Option<String>,
    /// `Unused` + `auto-delete` + git-clean ⇒ this item's deletion is
    /// MachineApplicable, so removing it (and cascading) is sound.
    pub removable: bool,
    pub diagnostic: Diagnostic,
}

pub(super) fn check(
    global: &UnusedPubConfig,
    per_crate: &HashMap<String, UnusedPubConfig>,
    fast: &FastModel,
    model: &SemanticModel,
) -> Vec<Diagnostic> {
    findings(global, per_crate, fast, model.pub_candidates())
        .into_iter()
        .map(|f| f.diagnostic)
        .collect()
}

/// The candidate-driven core of the lint — shared by the plain [`check`] (fed
/// `SemanticModel::pub_candidates`) and the `--fix` cascade (fed
/// `pub_candidates_excluding`). Returns per-candidate [`PubFinding`]s plus the
/// crate-level publish hints, in the same order [`check`] always emitted them.
pub(crate) fn findings(
    global: &UnusedPubConfig,
    per_crate: &HashMap<String, UnusedPubConfig>,
    fast: &FastModel,
    candidates: Vec<PubCandidate>,
) -> Vec<PubFinding> {
    let mut by_crate: BTreeMap<&str, Vec<&PubCandidate>> = BTreeMap::new();
    for c in &candidates {
        by_crate.entry(c.krate.as_str()).or_default().push(c);
    }
    // Within a crate, report in source order (file, then line) — the closest
    // stable analog of the syn module-tree walk order.
    for list in by_crate.values_mut() {
        list.sort_by_key(|c| {
            c.span
                .as_ref()
                .map(|s| (s.file.clone(), s.line))
                .unwrap_or_default()
        });
    }

    let mut out = Vec::new();
    for krate in fast.members() {
        // A per-crate `[crates.<name>.unused-pub]` wholesale-replaces the
        // global params for this crate; the glob sets / kind filter are built
        // from the resolved config, so they're computed per crate.
        let config = per_crate.get(&krate.name).unwrap_or(global);
        let crate_code = krate.code_name();
        if config
            .exclude_crates
            .iter()
            .any(|c| c == &krate.name || c == &crate_code)
        {
            continue;
        }
        let Some(cands) = by_crate.get(crate_code.as_str()) else {
            continue;
        };
        let kind_filter: Option<HashSet<&'static str>> = (!config.kinds.is_empty())
            .then(|| config.kinds.iter().map(|k| k.to_ir_kind()).collect());
        // A crate's library-public items are exempt as "external API surface"
        // only when the crate actually has out-of-workspace consumers — i.e.
        // it declares `publish = true` (or a registry list) — or the user
        // opted every crate in via `assume-all-public`.
        let exempt_external_api = config.assume_all_public
            || matches!(
                fast.resolved_publish(krate),
                Publish::ExplicitTrue | Publish::Registries(_)
            );
        let ctx = CheckCtx {
            root: fast.root(),
            target_directory: fast.target_directory(),
            crate_code: &crate_code,
            kind_filter: kind_filter.as_ref(),
            allowlist: build_glob_set(&config.allowlist).as_ref().cloned(),
            exclude_paths: build_glob_set(&config.exclude_paths).as_ref().cloned(),
            suppress_intra_crate: config.suppress_intra_crate,
            auto_delete: config.auto_delete,
            exempt_external_api,
        };
        let mut crate_findings = Vec::new();
        for cand in cands {
            if let Some(f) = check_candidate(cand, &ctx) {
                crate_findings.push(f);
            }
        }
        // When a *workspace-internal* crate accumulates several findings, the
        // likely cause is that it really is published — nudge the author
        // toward the one-line fix. Self-resolving: adding `publish = true`
        // exempts the items, so the findings and this hint both disappear.
        let threshold = config
            .publish_hint_threshold
            .unwrap_or(DEFAULT_PUBLISH_HINT_THRESHOLD);
        if !exempt_external_api && threshold > 0 && crate_findings.len() >= threshold {
            out.push(PubFinding {
                id: None,
                removable: false,
                diagnostic: publish_hint(krate, &crate_code, crate_findings.len()),
            });
        }
        out.extend(crate_findings);
    }
    out
}

struct CheckCtx<'a> {
    /// Workspace root — joins the IR's workspace-relative span files into the
    /// absolute paths diagnostics anchor to (matching the syn backend).
    root: &'a Path,
    /// Cargo's target directory — everything under it is build-generated.
    target_directory: &'a Path,
    crate_code: &'a str,
    kind_filter: Option<&'a HashSet<&'static str>>,
    allowlist: Option<GlobSet>,
    exclude_paths: Option<GlobSet>,
    suppress_intra_crate: bool,
    auto_delete: bool,
    /// Whether library-public items in this crate are exempt as external API
    /// surface (the crate is published, or `assume-all-public` is set).
    exempt_external_api: bool,
}

impl CheckCtx<'_> {
    fn abs_file(&self, span: &wl_ir::Span) -> PathBuf {
        self.root.join(&span.file)
    }
}

fn check_candidate(cand: &PubCandidate, ctx: &CheckCtx<'_>) -> Option<PubFinding> {
    if candidate_skipped_by_filters(cand, ctx) {
        return None;
    }
    let verdict = match cand.usage {
        // Cross-crate or dispatch/export-reached: provably in use — leave
        // alone. (Dispatch covers the `ffi_no_mangle_export` class syn
        // false-positived on.)
        PubUsage::CrossCrate | PubUsage::DispatchReached => return None,
        PubUsage::IntraCrate if ctx.suppress_intra_crate => return None,
        PubUsage::IntraCrate => PubVerdict::IntraCrate,
        PubUsage::Unused => PubVerdict::Unused,
    };
    let span = cand.span.as_ref()?;
    let (diagnostic, removable) = build_diagnostic(cand, ctx, span, verdict);
    Some(PubFinding {
        id: Some(cand.id.clone()),
        removable,
        diagnostic,
    })
}

/// Pure filter cascade — every reason to bail before composing a diagnostic,
/// mirroring the syn backend's `item_skipped_by_filters` (kind/visibility
/// pre-filters live in the assembler's candidate set already).
fn candidate_skipped_by_filters(cand: &PubCandidate, ctx: &CheckCtx<'_>) -> bool {
    // Findings target module-level and inherent-impl items. A trait-impl
    // item's visibility is trait-forced — no tighten surface, deletion breaks
    // the `impl` — so it is judged (dispatch reachability) but never flagged.
    if !matches!(
        cand.category,
        Category::ModuleLevel | Category::InherentImpl
    ) {
        return true;
    }
    // The crate root `main` of a bin target.
    if cand.name == "main" && cand.id.split("::").count() == 2 {
        return true;
    }
    // Build-generated code (`OUT_DIR` content spliced via `include!` lands
    // under cargo's target dir): analyzed, but never an author-editable
    // finding surface.
    if let Some(span) = &cand.span
        && ctx.abs_file(span).starts_with(ctx.target_directory)
    {
        return true;
    }
    if let Some(kf) = ctx.kind_filter
        && !kf.contains(cand.kind.as_str())
    {
        return true;
    }
    if let Some(al) = &ctx.allowlist
        && al.is_match(&cand.id)
    {
        return true;
    }
    if let Some(ex) = &ctx.exclude_paths
        && let Some(span) = &cand.span
        && ex.is_match(ctx.abs_file(span).to_string_lossy().as_ref())
    {
        return true;
    }
    // A re-export target is part of the crate's API regardless of publish
    // status — narrowing it would break the `pub use` (E0364 / E0365).
    if cand.reexport_target {
        return true;
    }
    // Named in the *public signature* of a more-visible item: tightening
    // would not compile (E0446 / `private_interfaces`). Publish-independent,
    // so it runs before the `exempt_external_api`-gated reachability guard.
    if cand.signature_exposed {
        return true;
    }
    // Library-public reachability only exempts the item when the crate is
    // treated as having external (out-of-workspace) consumers.
    if ctx.exempt_external_api && cand.externally_reachable {
        return true;
    }
    false
}

/// Build the rendered diagnostic, returning `(diagnostic, removable)` where
/// `removable` is `true` iff the structural fix is a **MachineApplicable
/// deletion** — the signal the cascade seeds its removed set from.
fn build_diagnostic(
    cand: &PubCandidate,
    ctx: &CheckCtx<'_>,
    span: &wl_ir::Span,
    verdict: PubVerdict,
) -> (Diagnostic, bool) {
    let kind_str = &cand.kind;
    let crate_code = ctx.crate_code;
    let (message, suggestion) = match verdict {
        PubVerdict::IntraCrate => (
            format!(
                "pub {kind_str} `{}` in crate `{crate_code}` is only used inside the crate",
                cand.name
            ),
            "consider `pub(crate)` to tighten visibility",
        ),
        PubVerdict::Unused => (
            format!(
                "pub {kind_str} `{}` in crate `{crate_code}` appears unused — consider removing",
                cand.name
            ),
            "remove the item or its `pub` visibility",
        ),
    };

    let file = ctx.abs_file(span);
    let builder = at_line(LintId::UnusedPub.id(), message, file.clone(), span.line)
        .help(suggestion)
        .note(
            "code compiled under configs outside `[engine] configs` and out-of-workspace consumers may cause false positives",
        );
    let (builder, removable) =
        apply_structural_fix(builder, cand, ctx.auto_delete, &file, span, verdict);
    (builder.build(), removable)
}

/// Structural fix policy — identical to the syn backend's:
///  - `IntraCrate` → `pub` → `pub(crate)`, `MachineApplicable` (the candidate
///    has an intra-crate referrer and has cleared every structural
///    must-stay-`pub` guard).
///  - `Unused` + `auto_delete` + git-tracked-clean → delete.
///  - `Unused` + `auto_delete` + dirty/untracked → deletion as
///    `MaybeIncorrect` plus an explanatory note.
///  - `Unused` without `auto_delete` → tighten as `MaybeIncorrect`: "unused"
///    still has residual blind spots (configs outside the matrix), so the
///    suggestion is shown but not auto-applied.
fn apply_structural_fix(
    builder: wl_diagnostic::builder::DiagnosticBuilder,
    cand: &PubCandidate,
    auto_delete: bool,
    file: &Path,
    span: &wl_ir::Span,
    verdict: PubVerdict,
) -> (wl_diagnostic::builder::DiagnosticBuilder, bool) {
    // The deletion surface is the WHOLE item (attrs/doc through body) — `span`
    // is rustc's `def_span`, only the signature, so deleting it would orphan a
    // function's body. `full_span` falls back to `span` only for the
    // no-editable-surface items that are never deletion candidates anyway.
    let delete_span = cand.full_span.as_ref().unwrap_or(span);
    if let Some((sugg, note)) = pick_deletion_fix(auto_delete, file, delete_span, verdict) {
        // Only a git-clean (recoverable) deletion is MachineApplicable — that
        // is the removal the cascade may build on.
        let removable = sugg.applicability == Applicability::MachineApplicable;
        let with_sugg = builder.suggestion(sugg);
        let builder = note.into_iter().fold(with_sugg, |b, reason| b.note(reason));
        return (builder, removable);
    }
    let builder = build_tighten_suggestion(cand, file, verdict)
        .into_iter()
        .fold(builder, |b, s| b.suggestion(s));
    // `pub(crate)` compiles but trips `dead_code` on the plain build — say
    // why `--fix` won't apply it.
    let builder = if verdict == PubVerdict::IntraCrate && cand.test_only {
        builder.note(
            "every use-site is test/bench cfg-gated: `pub(crate)` would trip `dead_code` \
             on the non-test build — gate the item `#[cfg(test)]`, move it into test code, \
             or delete it",
        )
    } else if verdict == PubVerdict::IntraCrate && cand.dead_members {
        builder.note(
            "this trait declares members nothing calls: `pub(crate)` un-exempts it from \
             `dead_code`, which will flag them — remove the unused members first",
        )
    } else {
        builder
    };
    (builder, false)
}

/// Build a suggestion that overwrites the item's visibility token with
/// `pub(crate)`. The byte range is the extractor's `vis_span` — rustc's own
/// token span mapped to on-disk offsets (see `wl_ir::Span`), verified
/// byte-exact against syn on every fixture the two share. `None` when there
/// is no editable token (macro-generated, trait-forced).
fn build_tighten_suggestion(
    cand: &PubCandidate,
    file: &Path,
    verdict: PubVerdict,
) -> Option<wl_diagnostic::Suggestion> {
    let span = cand.span.as_ref()?;
    let vis = cand.vis_span.as_ref()?;
    if vis.from_expansion {
        return None; // the token lives in a macro definition — not editable
    }
    let applicability = match verdict {
        // Reached only from test/bench cfg-gated code, or a trait with
        // never-called members: the narrow compiles but trips `dead_code` on
        // the plain build — shown, never machine-applied (the data-common
        // `is_days`/`fraction` and ChronoExt clusters from the 2026-07-05
        // LeaveDates validation).
        PubVerdict::IntraCrate if cand.test_only || cand.dead_members => {
            Applicability::MaybeIncorrect
        }
        PubVerdict::IntraCrate => Applicability::MachineApplicable,
        PubVerdict::Unused => Applicability::MaybeIncorrect,
    };
    // The existing visibility text for the rendered `-` diff line; falls back
    // to a placeholder if the file can't be read.
    let original = fs_err::read_to_string(file).ok().and_then(|src| {
        src.get(vis.lo as usize..vis.hi as usize)
            .map(str::to_string)
    });
    Some(wl_diagnostic::Suggestion {
        span: wl_diagnostic::Span {
            file: file.to_path_buf(),
            line_start: span.line,
            line_end: span.line,
            col_start: 1,
            col_end: 1,
            byte_start: vis.lo,
            byte_end: vis.hi,
        },
        message: "tighten to `pub(crate)`".into(),
        replacement: "pub(crate)".into(),
        applicability,
        original,
        // Filled in by `attach_pub_evidence` once the diagnostic is built.
    })
}

/// Pick a deletion suggestion when the user asked for one (`auto_delete`) and
/// the item is genuinely unused. Returns `None` to mean "fall back to the
/// tightening suggestion". The `Option<String>` second element carries the
/// "git-dirty file" caveat note when present.
pub(super) fn pick_deletion_fix(
    auto_delete: bool,
    file: &Path,
    span: &wl_ir::Span,
    verdict: PubVerdict,
) -> Option<(wl_diagnostic::Suggestion, Option<String>)> {
    if !auto_delete || verdict != PubVerdict::Unused {
        return None;
    }
    match delete_suggestion(file, span) {
        DeleteOutcome::Apply(s) => Some((s, None)),
        DeleteOutcome::Skip(s, reason) => Some((s, Some(reason))),
        DeleteOutcome::Unavailable => None,
    }
}

pub(super) enum DeleteOutcome {
    /// Git-tracked-clean: emit a MachineApplicable deletion suggestion.
    Apply(wl_diagnostic::Suggestion),
    /// Tracked-but-dirty or untracked: emit MaybeIncorrect so `--fix` passes
    /// over it, plus a reason note for the user.
    Skip(wl_diagnostic::Suggestion, String),
    /// File can't be read, degenerate range, etc. Fall back to the
    /// visibility-narrowing path.
    Unavailable,
}

pub(super) fn delete_suggestion(file: &Path, span: &wl_ir::Span) -> DeleteOutcome {
    let Ok(source) = fs_err::read_to_string(file) else {
        return DeleteOutcome::Unavailable;
    };
    let mut start = span.lo as usize;
    let mut end = (span.hi as usize).min(source.len());
    if start >= end {
        return DeleteOutcome::Unavailable;
    }
    // The extractor's `full_span` covers doc comments but NOT every attribute:
    // `#[cfg(...)]` is stripped from HIR in the unit where the item survives,
    // and an attribute-macro (`#[tracing::instrument]`) is consumed before HIR
    // exists — deleting the span as-is orphans those attrs onto whatever
    // follows (a syntax error before `}`, a silent re-target otherwise). Outer
    // attributes bind to the item below them, so extending the deletion
    // backward over contiguous `#[…]` blocks (and doc lines) is always sound.
    start = extend_over_preceding_attrs(&source, start);
    // Eat the item's leading indentation (horizontal whitespace back to the
    // line start) so a deleted nested item leaves no orphaned indent. Safe: it
    // only crosses spaces/tabs, never a newline or another item's text.
    let bytes = source.as_bytes();
    while start > 0 && matches!(bytes[start - 1], b' ' | b'\t') {
        start -= 1;
    }
    // The item text itself (sans the trailing newline the deletion also
    // eats), for the rendered `-` diff line.
    let original = source[start..end].to_string();
    if end < source.len() && source.as_bytes()[end] == b'\n' {
        end += 1;
    }
    // Also consume whole blank lines below the item: the item's surrounding
    // blank separators would otherwise stack into fmt-dirty residue
    // (`cargo fmt --check` fails on the fixed tree). The blank ABOVE survives
    // as the neighbors' separator; when a deletion run reaches EOF, the
    // applier trims the then-trailing blank lines (only it can see the merged
    // picture across adjacent deletions).
    end = eat_blank_lines(&source, end);
    let applicability = if is_file_clean_in_git(file) {
        Applicability::MachineApplicable
    } else {
        Applicability::MaybeIncorrect
    };
    let suggestion = wl_diagnostic::Suggestion {
        span: wl_diagnostic::Span {
            file: file.to_path_buf(),
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
        original: Some(original),
        // Filled in by `attach_pub_evidence` once the diagnostic is built.
    };
    if applicability == Applicability::MachineApplicable {
        DeleteOutcome::Apply(suggestion)
    } else {
        DeleteOutcome::Skip(
            suggestion,
            format!(
                "file `{}` is untracked or has uncommitted changes; `--fix` will not auto-delete (commit first or use `git stash`)",
                file.display()
            ),
        )
    }
}

/// Extend a deletion's start backward over the contiguous stack of outer
/// attributes (`#[…]`, multiline OK, string literals skipped) and doc-comment
/// lines directly above it. Inner attributes (`#![…]`) bind to the enclosing
/// module, never the item — the scanner refuses them. On any ambiguity the
/// scanner stops: the fallback is today's behavior (attr left in place).
pub(super) fn extend_over_preceding_attrs(source: &str, mut start: usize) -> usize {
    let bytes = source.as_bytes();
    loop {
        // Probe backward over whitespace to the last non-ws char.
        let mut i = start;
        while i > 0 && bytes[i - 1].is_ascii_whitespace() {
            i -= 1;
        }
        if i == 0 {
            return start;
        }
        if bytes[i - 1] == b']' {
            match match_attr_backward(source, i) {
                Some(hash) => {
                    start = hash;
                    continue;
                }
                None => return start,
            }
        }
        // A doc-comment line directly above (`///` — sugar for a doc attr;
        // usually already inside `full_span`, but a macro-consumed item can
        // lose it too). Consume the whole line and keep walking.
        let line_start = source[..i].rfind('\n').map_or(0, |p| p + 1);
        if source[line_start..i].trim_start().starts_with("///") {
            start = line_start;
            continue;
        }
        return start;
    }
}

/// Backward-match one attribute block: `end` is the index just past a `]`;
/// returns the index of the opening `#` when the bracket run balances to an
/// OUTER `#[`. String literals are skipped (a `]` or `[` inside `"…"` is
/// text), with backslash-parity escape handling. `None` on inner attrs
/// (`#![`), unbalanced brackets, or any construct the scanner doesn't model
/// (e.g. raw strings) — never a wrong match, just no extension.
fn match_attr_backward(source: &str, end: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut depth = 0usize;
    let mut i = end;
    while i > 0 {
        i -= 1;
        match bytes[i] {
            b'"' => {
                // Skip backward to the opening quote (an unescaped `"`).
                loop {
                    i = source[..i].rfind('"')?;
                    let backslashes = source[..i]
                        .bytes()
                        .rev()
                        .take_while(|&b| b == b'\\')
                        .count();
                    if backslashes % 2 == 0 {
                        break;
                    }
                }
                // A raw string's hash fence (`r#"…"#`) would leave a stray
                // `#` here and the bracket math can't be trusted — bail.
                if i > 0 && bytes[i - 1] == b'#' {
                    return None;
                }
            }
            b']' => depth += 1,
            b'[' => {
                depth -= 1;
                if depth == 0 {
                    return match i.checked_sub(1) {
                        // `#![…]` is an inner attribute — not the item's.
                        Some(h) if bytes[h] == b'!' => None,
                        Some(h) if bytes[h] == b'#' => Some(h),
                        _ => None,
                    };
                }
            }
            _ => {}
        }
    }
    None
}

/// Consume whole whitespace-only lines starting at `end` (which sits at a
/// line start after the deletion ate its trailing newline).
pub(super) fn eat_blank_lines(source: &str, end: usize) -> usize {
    eat_blank_lines_bytes(source.as_bytes(), end)
}

pub(super) fn eat_blank_lines_bytes(bytes: &[u8], mut end: usize) -> usize {
    loop {
        let mut i = end;
        while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\r') {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'\n' {
            end = i + 1;
        } else {
            return end;
        }
    }
}

/// `true` iff `path` is tracked by git AND has no uncommitted changes.
/// Returns `false` if we can't determine the state — the safer default is to
/// downgrade the suggestion's applicability so `--fix` skips it.
fn is_file_clean_in_git(path: &Path) -> bool {
    let ls = crate::git::command(Path::new("."))
        .args(["ls-files", "--error-unmatch", "--"])
        .arg(path)
        .output();
    let Ok(out) = ls else { return false };
    if !out.status.success() {
        return false;
    }
    let st = crate::git::command(Path::new("."))
        .args(["status", "--porcelain", "--"])
        .arg(path)
        .output();
    let Ok(out) = st else { return false };
    if !out.status.success() {
        return false;
    }
    out.stdout.is_empty()
}

/// One crate-level hint suggesting `publish = true` for an internal crate
/// that produced `count` findings.
fn publish_hint(krate: &CrateInfo, crate_code: &str, count: usize) -> Diagnostic {
    at_crate(
        LintId::UnusedPub.id(),
        format!("crate `{crate_code}` has {count} public items unused within the workspace"),
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

pub(super) fn build_glob_set(patterns: &[GlobPattern]) -> Option<GlobSet> {
    if patterns.is_empty() {
        return None;
    }
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(pattern.compiled().clone());
    }
    Some(
        builder
            .build()
            .unwrap_or_else(|e| crate::util::fail(format!("failed to build glob filter: {e}"))),
    )
}
