//! Visibility tightening — pub items that are only used inside their own
//! crate could be `pub(crate)` instead.
//!
//! Walks every workspace member's `pub` items and checks whether the item's
//! canonical path is referenced from any module in a *different* workspace
//! crate. Items with no cross-crate references are flagged as candidates
//! for `pub(crate)`. References are resolved through Tier 2.5's pub-use
//! chain index, so re-exported items still count as used when the
//! re-export itself is consumed cross-crate.
//!
//! Known limitations (v1) — documented in fixtures under
//! `known_false_positives/`:
//!
//! - Items referenced via fully-qualified path (`my_crate::Foo::bar()`)
//!   instead of a `use` statement are not tracked.
//! - Items referenced only inside macro bodies are not tracked.
//! - Trait methods dispatched via `dyn Trait` are not tracked (would need
//!   type inference).

use std::collections::HashSet;

use syn_workspace::{Item, ItemKind, Module, ResolvedPath, SourceSpan, Visibility, Workspace};

use crate::diagnostic::Diagnostic;
use crate::diagnostic::Suggestion;
use crate::diagnostic::builder::at_line;

pub const LINT: &str = crate::lints::LintId::Visibility.id();

pub fn check(workspace: &Workspace) -> Vec<Diagnostic> {
    let cross_crate_refs = collect_cross_crate_refs(workspace);
    let mut diagnostics = Vec::new();

    // Only inspect each member's primary unit — tests/benches/examples
    // legitimately use `pub` for cross-test plumbing, and flagging them
    // would amount to noise.
    for (krate, target) in workspace.primary_units() {
        let code_name = krate.code_name();
        // Items reachable via macro-rules expansion should not be flagged
        // even if no `use` binding mentions them. Layer 1 autodetect
        // collects these per defining crate; here we union the refs from
        // every crate that could plausibly invoke a macro touching this
        // crate's items (its own macros + macros from every dependent
        // crate). See `macro_implicit_refs_for` for the full rule.
        let macro_refs = workspace.macro_implicit_refs_for(krate);
        for (module, item) in target.root.walk_items() {
            if let Some(d) = check_item(module, item, &code_name, &cross_crate_refs, &macro_refs) {
                diagnostics.push(d);
            }
        }
    }
    diagnostics
}

fn check_item(
    module: &Module,
    item: &Item,
    crate_code_name: &str,
    cross_crate_refs: &HashSet<ResolvedPath>,
    macro_refs: &HashSet<ResolvedPath>,
) -> Option<Diagnostic> {
    if !checkable(item) {
        return None;
    }
    if item.visibility != Visibility::Public {
        return None;
    }
    // The crate root's own `pub fn main()` is special — bins need pub
    // for cargo's entry-point resolution machinery.
    if item.name == "main" && module.canonical.segments().len() == 1 {
        return None;
    }
    if cross_crate_refs.contains(&item.canonical) {
        return None;
    }
    // Suppress if the item is reachable through any workspace
    // `macro_rules!` body — that's a real cross-crate use channel
    // even if no explicit `use` binding points at it.
    if macro_refs.contains(&item.canonical) {
        return None;
    }

    let span = item.source.as_ref()?;
    let msg = format!(
        "pub `{}` in crate `{}` is not referenced from any other workspace crate",
        item.name, crate_code_name,
    );
    let mut builder = at_line(LINT, msg, span.file.clone(), span.line)
        .help("tighten to `pub(crate)` if this item is intentionally crate-internal")
        .note("references via fully-qualified path, trait dispatch, or proc-macro bodies are not tracked");
    if let Some(s) = build_tighten_suggestion(span) {
        builder = builder.suggestion(s);
    }
    Some(builder.build())
}

/// Locate the `pub` token within an item's span and return its byte range,
/// or `None` if the heuristic can't pin it down. When this returns `None`
/// the diagnostic still fires — `--fix` simply has nothing to apply and
/// leaves the file alone for the human to address. Cases we punt on:
///
/// - Item starts with `#[...]` attributes — the `pub` keyword is deeper
///   inside the span and disambiguating it from `#[allow(...)]` or
///   attribute argument tokens needs syn rather than a string scan.
/// - The keyword we find is `pub(...)` already (`pub(super)`,
///   `pub(in path)`) — those are explicit author choices and the lint
///   shouldn't downgrade them.
/// - We can't read the source file (deleted on disk, permissions, …).
pub(crate) fn build_tighten_suggestion(span: &SourceSpan) -> Option<Suggestion> {
    if span.byte_start == 0 && span.byte_end == 0 {
        return None;
    }
    let source = fs_err::read_to_string(&span.file).ok()?;
    let start = span.byte_start as usize;
    let end = (span.byte_end as usize).min(source.len());
    if start >= end {
        return None;
    }
    let slice = source.get(start..end)?;
    // Bail if the span starts with an outer attribute — the `pub` keyword
    // is buried among tokens we can't safely identify by string scan.
    if slice.starts_with('#') {
        return None;
    }
    // Find first `pub` at a word boundary.
    let pub_offset = find_word_boundary_pub(slice)?;
    let after_pub = slice.get(pub_offset + 3..)?;
    // Already `pub(...)` — leave it alone.
    if after_pub.starts_with('(') {
        return None;
    }
    // Require a whitespace separator after `pub` so we don't grab
    // identifiers like `public_field`.
    if !after_pub
        .chars()
        .next()
        .map(char::is_whitespace)
        .unwrap_or(false)
    {
        return None;
    }
    let abs_start = (start + pub_offset) as u32;
    let abs_end = abs_start + 3;
    Some(Suggestion {
        span: crate::diagnostic::Span {
            file: span.file.clone(),
            line_start: span.line,
            line_end: span.line,
            col_start: 1,
            col_end: 1,
            byte_start: abs_start,
            byte_end: abs_end,
        },
        message: "tighten to `pub(crate)`".into(),
        replacement: "pub(crate)".into(),
        applicability: crate::diagnostic::Applicability::MachineApplicable,
    })
}

/// Find `pub` in `slice` at the first position where it's a standalone
/// keyword (preceded by whitespace, start of slice, or punctuation —
/// `pub` cannot follow an identifier character).
fn find_word_boundary_pub(slice: &str) -> Option<usize> {
    let bytes = slice.as_bytes();
    for (i, w) in bytes.windows(3).enumerate() {
        if w == b"pub" {
            let before_ok =
                i == 0 || !matches!(bytes[i - 1], b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_');
            if before_ok {
                return Some(i);
            }
        }
    }
    None
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
    )
}

fn collect_cross_crate_refs(workspace: &Workspace) -> HashSet<ResolvedPath> {
    // Walk the resolver's per-crate references index (use bindings +
    // regular code paths + macro_rules! body refs), follow each path
    // through any `pub use` chain, and keep only paths that point to a
    // different crate than the referring one. This subsumes the old
    // use-binding-only walk while also catching fully-qualified path
    // references inside function bodies (e.g. `lib_a::Button` inside an
    // rsx! body that has no `use` statement).
    let mut refs = HashSet::new();
    for (referring_crate, path) in workspace.iter_references() {
        let canonical = workspace.resolve_canonical(path);
        if canonical.crate_name() != Some(referring_crate) {
            refs.insert(canonical);
        }
    }
    refs
}
