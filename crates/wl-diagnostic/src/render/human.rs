//! Clippy-style human renderer.
//!
//! Format per diagnostic:
//!
//! ```text
//! warning: <message>
//!  --> <file>:<line>:<col>
//!   |
//!   = help: <help>
//!   = note: <note>
//! help: <suggestion message>
//!   |
//! L - <old line if line-replacement>
//! L + <new line>
//!   |
//!   = note: `#[warn(workspace_lint::<lint_ident>)]` on by default
//! ```
//!
//! Followed by a final summary line: `warning: workspace-lint generated N
//! warnings`.

use std::collections::HashSet;
use std::io::{self, Write};

use super::display_path;
use crate::{Diagnostic, Level, SilenceAnchor};

/// Run-scoped dedup so a lint's explanatory boilerplate prints once instead of
/// once per finding. The `= help:` lines and per-finding suggestions are real,
/// per-finding signal and are never deduped; only the repeated `= note:`
/// caveats and the "if intentional, silence with:" trailer collapse.
#[derive(Default)]
struct RenderState {
    /// `(lint id, note text)` already emitted — a given caveat shows once per
    /// lint, but a genuinely different note still prints.
    seen_notes: HashSet<(String, String)>,
    /// Lint ids whose silence hint + `on by default` trailer were already shown.
    seen_silence: HashSet<String>,
}

pub(crate) fn write(diagnostics: &[Diagnostic], out: &mut dyn Write) -> io::Result<()> {
    let mut state = RenderState::default();
    for d in diagnostics {
        write_one_stateful(d, &mut state, out)?;
        writeln!(out)?;
    }
    write_summary(diagnostics, out)?;
    Ok(())
}

/// Render a single diagnostic in isolation (no cross-diagnostic dedup). Reached
/// via [`super::render_one`]; the multi-diagnostic path goes through [`write`],
/// which shares one [`RenderState`] across the run.
pub(crate) fn write_one(d: &Diagnostic, out: &mut dyn Write) -> io::Result<()> {
    write_one_stateful(d, &mut RenderState::default(), out)
}

fn write_one_stateful(
    d: &Diagnostic,
    state: &mut RenderState,
    out: &mut dyn Write,
) -> io::Result<()> {
    writeln!(out, "{}: {}", d.level.as_str(), d.message)?;

    if let Some(loc) = location_line(d) {
        writeln!(out, " --> {loc}")?;
        writeln!(out, "  |")?;
    }

    for help in &d.helps {
        writeln!(out, "  = help: {help}")?;
    }
    let lint_id = d.lint.to_string();
    for note in &d.notes {
        if state.seen_notes.insert((lint_id.clone(), note.clone())) {
            writeln!(out, "  = note: {note}")?;
        }
    }

    for suggestion in &d.suggestions {
        write_suggestion_block(suggestion, out)?;
    }

    if let Some(silence) = d.silence_suggestion()
        && state.seen_silence.insert(lint_id)
    {
        write_suggestion_block(&silence, out)?;
        writeln!(
            out,
            "  = note: `#[{}({})]` on by default",
            match d.level {
                Level::Warn => "warn",
                Level::Deny => "deny",
            },
            format_attr_lint(&d.lint_ident()),
        )?;
    }
    Ok(())
}

fn location_line(d: &Diagnostic) -> Option<String> {
    if let Some(span) = &d.primary {
        return Some(format!(
            "{}:{}:{}",
            display_path(&span.file),
            span.line_start,
            span.col_start
        ));
    }
    match &d.silence_anchor {
        SilenceAnchor::Line { file, line } => Some(format!("{}:{}:1", display_path(file), line)),
        SilenceAnchor::File { file } => Some(format!("{}:1:1", display_path(file))),
        SilenceAnchor::Crate { manifest_dir } => {
            Some(format!("{}/Cargo.toml:1:1", display_path(manifest_dir)))
        }
        SilenceAnchor::Workspace => None,
    }
}

fn write_suggestion_block(s: &crate::Suggestion, out: &mut dyn Write) -> io::Result<()> {
    writeln!(out, "help: {}", s.message)?;
    writeln!(out, "  |")?;
    if s.replacement.is_empty() {
        // Pure deletion — show old line(s) as -, nothing as +
        write_removed(s, out)?;
    } else if s.span.byte_start == s.span.byte_end {
        // Pure insertion — show inserted text only with `+`
        for line in s.replacement.lines() {
            writeln!(out, "{} + {line}", s.span.line_start)?;
        }
    } else {
        // Replacement — old line(s) shown as -, new as +
        write_removed(s, out)?;
        for line in s.replacement.lines() {
            writeln!(out, "{} + {line}", s.span.line_start)?;
        }
    }
    writeln!(out, "  |")?;
    Ok(())
}

/// Emit the `-` side of a deletion/replacement diff. With the captured source
/// text (`s.original`) this is the real line — clippy-style. A single-line span
/// prints in full; a multi-line span prints its first line plus a `…`
/// continuation so a large item (e.g. an `unused-pub` deletion) doesn't dump
/// dozens of lines. Without captured text we fall back to a bare `…`.
fn write_removed(s: &crate::Suggestion, out: &mut dyn Write) -> io::Result<()> {
    match s.original.as_deref() {
        Some(text) if !text.contains('\n') => writeln!(out, "{} - {text}", s.span.line_start),
        Some(text) => {
            let first = text.lines().next().unwrap_or_default();
            writeln!(out, "{} - {first}", s.span.line_start)?;
            writeln!(out, "  …")
        }
        None => writeln!(out, "{} - …", s.span.line_start),
    }
}

fn format_attr_lint(ident: &str) -> String {
    format!("workspace_lint::{ident}")
}

fn write_summary(diagnostics: &[Diagnostic], out: &mut dyn Write) -> io::Result<()> {
    if diagnostics.is_empty() {
        writeln!(out, "workspace-lint: all passed")?;
        return Ok(());
    }
    let warns = diagnostics
        .iter()
        .filter(|d| d.level == Level::Warn)
        .count();
    let denies = diagnostics
        .iter()
        .filter(|d| d.level == Level::Deny)
        .count();
    // Match the predicate `fix::run` uses: only byte-range structural
    // replacements count, since `--fix` will never apply silence inserts.
    let machine_fixes = diagnostics
        .iter()
        .flat_map(|d| &d.suggestions)
        .filter(|s| {
            s.applicability == crate::Applicability::MachineApplicable
                && s.span.byte_end > s.span.byte_start
        })
        .count();

    let mut parts = Vec::new();
    if warns > 0 {
        parts.push(format!("{warns} warning{}", plural(warns)));
    }
    if denies > 0 {
        parts.push(format!("{denies} error{}", plural(denies)));
    }
    let head = parts.join(", ");
    if machine_fixes > 0 {
        writeln!(
            out,
            "workspace-lint: generated {head} (run `workspace-lint --fix` to apply {machine_fixes} suggestion{})",
            plural(machine_fixes)
        )?;
    } else {
        writeln!(out, "workspace-lint: generated {head}")?;
    }
    Ok(())
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::{at_file, at_workspace};

    fn render_one(d: &Diagnostic) -> String {
        let mut buf = Vec::new();
        write_one(d, &mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    fn render_all(d: &[Diagnostic]) -> String {
        let mut buf = Vec::new();
        write(d, &mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn header_uses_warning_for_warn() {
        let d = at_workspace("workspace-lint::centralized-deps", "msg").build();
        let s = render_one(&d);
        assert!(s.starts_with("warning: msg"));
    }

    #[test]
    fn header_uses_error_for_deny() {
        let d = at_workspace("workspace-lint::centralized-deps", "msg")
            .level(Level::Deny)
            .build();
        let s = render_one(&d);
        assert!(s.starts_with("error: msg"));
    }

    #[test]
    fn location_line_for_file_anchor() {
        let d = at_file("workspace-lint::file-size", "x", "src/lib.rs").build();
        let s = render_one(&d);
        assert!(s.contains(" --> src/lib.rs:1:1"));
    }

    #[test]
    fn workspace_anchor_omits_location_line() {
        let d = at_workspace("workspace-lint::centralized-deps", "x").build();
        let s = render_one(&d);
        assert!(!s.contains(" --> "));
    }

    #[test]
    fn helps_and_notes_appear_with_correct_prefix() {
        let d = at_workspace("workspace-lint::x", "x")
            .help("split your file")
            .note("see README")
            .build();
        let s = render_one(&d);
        assert!(s.contains("  = help: split your file"));
        assert!(s.contains("  = note: see README"));
    }

    #[test]
    fn silence_suggestion_renders_as_diff() {
        let d = at_file("workspace-lint::file-size", "x", "src/lib.rs").build();
        let s = render_one(&d);
        assert!(s.contains("help: if intentional, silence with:"));
        assert!(s.contains("workspace_lint::expect!(file_size);"));
        assert!(s.contains("on by default"));
    }

    #[test]
    fn summary_pluralizes_correctly() {
        let one = vec![at_workspace("x", "y").build()];
        let two = vec![
            at_workspace("x", "y").build(),
            at_workspace("x", "y").build(),
        ];
        let s_one = render_all(&one);
        let s_two = render_all(&two);
        assert!(s_one.contains("1 warning") && !s_one.contains("warnings"));
        assert!(s_two.contains("2 warnings"));
    }

    #[test]
    fn empty_diagnostics_says_all_passed() {
        let s = render_all(&[]);
        assert!(s.contains("all passed"));
    }
}
