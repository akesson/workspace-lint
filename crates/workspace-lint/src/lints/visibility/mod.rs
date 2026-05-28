//! Visibility tightening — `pub` items only used inside their own crate
//! could be `pub(crate)` instead.
//!
//! Walks every workspace member's `pub` items and checks whether the item's
//! canonical path is referenced from any module in a *different* workspace
//! crate. Items with no cross-crate references are flagged.
//!
//! Known limitations (v1):
//!
//! - Items referenced via fully-qualified path (`my_crate::Foo::bar()`)
//!   instead of a `use` statement are not tracked.
//! - Items referenced only inside macro bodies are not tracked.
//! - Trait methods dispatched via `dyn Trait` are not tracked (would need
//!   type inference).

use std::collections::HashSet;

use syn_workspace::{
    Item, ItemKind, Module, ResolvedPath, SourceSpan, Visibility as SynVisibility, Workspace,
};

use crate::diagnostic::Diagnostic;
use crate::diagnostic::Suggestion;
use crate::diagnostic::builder::at_line;
use crate::lints::{Lint, LintContext, LintId, Requirements};

pub struct Visibility;

impl Visibility {
    pub fn new() -> Self {
        Self
    }
}

impl Default for Visibility {
    fn default() -> Self {
        Self::new()
    }
}

impl Lint for Visibility {
    fn id(&self) -> LintId {
        LintId::Visibility
    }

    fn requirements(&self) -> Requirements {
        Requirements {
            needs_workspace: true,
        }
    }

    fn check(&self, cx: &LintContext<'_>) -> Vec<Diagnostic> {
        let workspace = cx
            .workspace
            .expect("visibility lint requires Workspace (Requirements::needs_workspace)");
        check(workspace)
    }
}

pub fn check(workspace: &Workspace) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    // Only inspect each member's primary unit — tests/benches/examples
    // legitimately use `pub` for cross-test plumbing.
    for (krate, target) in workspace.primary_units() {
        let code_name = krate.code_name();
        let macro_refs = workspace.macro_implicit_refs_for(krate);
        for (module, item) in target.root.walk_items() {
            if let Some(d) = check_item(workspace, module, item, &code_name, &macro_refs) {
                diagnostics.push(d);
            }
        }
    }
    diagnostics
}

fn check_item(
    workspace: &Workspace,
    module: &Module,
    item: &Item,
    crate_code_name: &str,
    macro_refs: &HashSet<ResolvedPath>,
) -> Option<Diagnostic> {
    if !item.kind.is_definition() || matches!(item.kind, ItemKind::Macro) {
        return None;
    }
    if item.visibility != SynVisibility::Public {
        return None;
    }
    if item.name == "main" && module.canonical.segments().len() == 1 {
        return None;
    }
    let cross_crate_used = workspace
        .referring_crates(&item.canonical)
        .is_some_and(|set| set.iter().any(|c| c != crate_code_name));
    if cross_crate_used {
        return None;
    }
    if macro_refs.contains(&item.canonical) {
        return None;
    }

    let span = item.source.as_ref()?;
    let msg = format!(
        "pub `{}` in crate `{}` is not referenced from any other workspace crate",
        item.name, crate_code_name,
    );
    let mut builder = at_line(LintId::Visibility.id(), msg, span.file.clone(), span.line)
        .help("tighten to `pub(crate)` if this item is intentionally crate-internal")
        .note("references via fully-qualified path, trait dispatch, or proc-macro bodies are not tracked");
    if let Some(s) = build_tighten_suggestion(span) {
        builder = builder.suggestion(s);
    }
    Some(builder.build())
}

/// Locate the `pub` token within an item's span and return its byte range,
/// or `None` if the heuristic can't pin it down. When this returns `None`
/// the diagnostic still fires — `--fix` simply has nothing to apply.
///
/// Cases punted on: items starting with attributes (the `pub` keyword is
/// deeper inside the span), items already `pub(...)`, files we can't read.
pub(crate) fn build_tighten_suggestion(span: &SourceSpan) -> Option<Suggestion> {
    let range = span.byte_range.clone()?;
    let source = fs_err::read_to_string(&span.file).ok()?;
    let start = range.start as usize;
    let end = (range.end as usize).min(source.len());
    if start >= end {
        return None;
    }
    let slice = source.get(start..end)?;
    if slice.starts_with('#') {
        return None;
    }
    let pub_offset = find_word_boundary_pub(slice)?;
    let after_pub = slice.get(pub_offset + 3..)?;
    if after_pub.starts_with('(') {
        return None;
    }
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
/// keyword (preceded by whitespace, start of slice, or punctuation).
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
