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

use std::io::{self, Write};

use super::display_path;
use crate::diagnostic::{Diagnostic, Level, SilenceAnchor};

pub fn write(diagnostics: &[Diagnostic], out: &mut dyn Write) -> io::Result<()> {
    for d in diagnostics {
        write_one(d, out)?;
        writeln!(out)?;
    }
    write_summary(diagnostics, out)?;
    Ok(())
}

pub fn write_one(d: &Diagnostic, out: &mut dyn Write) -> io::Result<()> {
    writeln!(out, "{}: {}", d.level.as_str(), d.message)?;

    if let Some(loc) = location_line(d) {
        writeln!(out, " --> {loc}")?;
        writeln!(out, "  |")?;
    }

    for help in &d.helps {
        writeln!(out, "  = help: {help}")?;
    }
    for note in &d.notes {
        writeln!(out, "  = note: {note}")?;
    }

    for suggestion in &d.suggestions {
        write_suggestion_block(suggestion, out)?;
    }

    if let Some(silence) = d.silence_suggestion() {
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

fn write_suggestion_block(
    s: &crate::diagnostic::Suggestion,
    out: &mut dyn Write,
) -> io::Result<()> {
    writeln!(out, "help: {}", s.message)?;
    writeln!(out, "  |")?;
    if s.replacement.is_empty() {
        // Pure deletion — show old line as -, nothing as +
        writeln!(out, "{} - …", s.span.line_start)?;
    } else if s.span.byte_start == s.span.byte_end {
        // Pure insertion — show inserted text only with `+`
        for line in s.replacement.lines() {
            writeln!(out, "{} + {line}", s.span.line_start)?;
        }
    } else {
        // Replacement — old line shown as -, new as +
        writeln!(out, "{} - <existing>", s.span.line_start)?;
        for line in s.replacement.lines() {
            writeln!(out, "{} + {line}", s.span.line_start)?;
        }
    }
    writeln!(out, "  |")?;
    Ok(())
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
    let machine_fixes = diagnostics
        .iter()
        .flat_map(|d| &d.suggestions)
        .filter(|s| s.applicability == crate::diagnostic::Applicability::MachineApplicable)
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
    use crate::diagnostic::builder::{at_file, at_workspace};

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
        assert!(s.contains("workspace_lint::allow!(file_size);"));
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
