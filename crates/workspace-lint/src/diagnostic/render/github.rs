//! GitHub Actions workflow-command renderer.
//!
//! Emits one line per diagnostic in the format
//! `::warning file=<path>,line=<n>,col=<n>,title=<lint>::<message>` (or
//! `::error` for `Level::Deny`). Picked up by GitHub Actions to annotate PR
//! diffs inline. See:
//! <https://docs.github.com/en/actions/using-workflows/workflow-commands-for-github-actions#setting-a-warning-message>

use std::io::{self, Write};

use super::display_path;
use crate::diagnostic::{Diagnostic, SilenceAnchor};

pub(crate) fn write(diagnostics: &[Diagnostic], out: &mut dyn Write) -> io::Result<()> {
    for d in diagnostics {
        write_one(d, out)?;
    }
    Ok(())
}

pub(crate) fn write_one(d: &Diagnostic, out: &mut dyn Write) -> io::Result<()> {
    // GitHub's `::warning`/`::error` command names coincide with rustc's level
    // strings (see [`Level::as_str`]).
    let command = d.level.as_str();
    let (file, line, col) = location(d);
    let file = escape_property(&file);
    let title = escape_property(&d.lint);
    let message = escape_data(&d.message);

    writeln!(
        out,
        "::{command} file={file},line={line},col={col},title={title}::{message}"
    )
}

fn location(d: &Diagnostic) -> (String, u32, u32) {
    if let Some(span) = &d.primary {
        return (display_path(&span.file), span.line_start, span.col_start);
    }
    match &d.silence_anchor {
        SilenceAnchor::Line { file, line } => (display_path(file), *line, 1),
        SilenceAnchor::File { file } => (display_path(file), 1, 1),
        SilenceAnchor::Crate { manifest_dir } => {
            (display_path(&manifest_dir.join("Cargo.toml")), 1, 1)
        }
        SilenceAnchor::Workspace => ("Cargo.toml".to_string(), 1, 1),
    }
}

/// Escape `,`, `:`, `\n`, `\r`, `%` for use in a property value (`file=…`).
fn escape_property(s: &str) -> String {
    s.replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
        .replace(':', "%3A")
        .replace(',', "%2C")
}

/// Escape `\n`, `\r`, `%` for use in the trailing message after `::`.
fn escape_data(s: &str) -> String {
    s.replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostic::Level;
    use crate::diagnostic::builder::{at_file, at_line, at_workspace};

    fn render_one(d: &Diagnostic) -> String {
        let mut buf = Vec::new();
        write_one(d, &mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn warn_emits_warning_command() {
        let d = at_file("workspace-lint::file-size", "msg", "src/lib.rs").build();
        let s = render_one(&d);
        assert!(s.starts_with("::warning "));
    }

    #[test]
    fn deny_emits_error_command() {
        let d = at_file("workspace-lint::file-size", "msg", "src/lib.rs")
            .level(Level::Deny)
            .build();
        let s = render_one(&d);
        assert!(s.starts_with("::error "));
    }

    #[test]
    fn file_anchor_carries_file_line_col() {
        let d = at_file("workspace-lint::file-size", "msg", "src/lib.rs").build();
        let s = render_one(&d);
        assert!(s.contains("file=src/lib.rs"));
        assert!(s.contains(",line=1,"));
        assert!(s.contains(",col=1,"));
    }

    #[test]
    fn line_anchor_carries_specific_line() {
        let d = at_line("workspace-lint::unused-pub", "msg", "src/lib.rs", 42).build();
        let s = render_one(&d);
        assert!(s.contains(",line=42,"));
    }

    #[test]
    fn workspace_anchor_falls_back_to_root_cargo_toml() {
        let d = at_workspace("workspace-lint::centralized-deps", "msg").build();
        let s = render_one(&d);
        assert!(s.contains("file=Cargo.toml"));
        assert!(s.contains(",line=1,"));
    }

    #[test]
    fn title_carries_lint_id() {
        let d = at_workspace("workspace-lint::centralized-deps", "msg").build();
        let s = render_one(&d);
        assert!(s.contains("title=workspace-lint%3A%3Acentralized-deps"));
    }

    #[test]
    fn message_after_double_colon() {
        let d = at_workspace("workspace-lint::x", "hello world").build();
        let s = render_one(&d);
        let suffix = s.split_once("::").unwrap().1.split_once("::").unwrap().1;
        assert_eq!(suffix.trim_end_matches('\n'), "hello world");
    }

    #[test]
    fn escapes_percent_and_newline_in_message() {
        let d = at_workspace("x", "with\nnewline%and%percent").build();
        let s = render_one(&d);
        assert!(s.contains("with%0Anewline%25and%25percent"));
    }

    #[test]
    fn escapes_commas_in_file_path() {
        let d = at_file("x", "m", "weird,name.rs").build();
        let s = render_one(&d);
        assert!(s.contains("file=weird%2Cname.rs"));
    }
}
