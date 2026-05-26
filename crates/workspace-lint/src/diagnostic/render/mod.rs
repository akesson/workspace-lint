//! Renderers for [`Diagnostic`]s.
//!
//! Three formats:
//! - [`human`]: clippy-style text with diff-style suggestions.
//! - [`json`]: rustc-compatible JSON, one object per line.
//! - [`github`]: GitHub Actions workflow commands for PR annotations.

use std::io::{self, Write};
use std::path::Path;

use super::Diagnostic;

pub mod github;
pub mod human;
pub mod json;

/// Display a `Path` with forward-slash separators regardless of host OS.
/// Every render backend (human, json, github) feeds paths to consumers that
/// treat `/` as canonical: GitHub Actions annotation `file=` properties,
/// editor jump-to-file on rustc-JSON, and the human `file:line:col` header
/// that gets copy-pasted into terminals. Backslashes from Windows
/// `Path::display()` would break those — so normalize at the render boundary.
pub(crate) fn display_path(p: &Path) -> String {
    let s = p.display().to_string();
    if std::path::MAIN_SEPARATOR == '\\' {
        s.replace('\\', "/")
    } else {
        s
    }
}

/// Output format selected by the `--message-format` flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Format {
    #[default]
    Human,
    Json,
    Github,
}

impl Format {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "human" => Ok(Self::Human),
            "json" => Ok(Self::Json),
            "github" => Ok(Self::Github),
            other => Err(format!(
                "unknown --message-format `{other}` (expected `human`, `json`, or `github`)"
            )),
        }
    }
}

/// Render every diagnostic plus the trailing summary (for `human`) to a
/// writer. Returns the number of `Deny`-level diagnostics, which the caller
/// uses to set the process exit code.
pub fn render(
    format: Format,
    diagnostics: &[Diagnostic],
    out: &mut dyn Write,
) -> io::Result<usize> {
    let deny_count = diagnostics
        .iter()
        .filter(|d| d.level == super::Level::Deny)
        .count();
    match format {
        Format::Human => human::write(diagnostics, out)?,
        Format::Json => json::write(diagnostics, out)?,
        Format::Github => github::write(diagnostics, out)?,
    }
    Ok(deny_count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_known_formats() {
        assert_eq!(Format::parse("human"), Ok(Format::Human));
        assert_eq!(Format::parse("json"), Ok(Format::Json));
        assert_eq!(Format::parse("github"), Ok(Format::Github));
    }

    #[test]
    fn display_path_uses_forward_slashes_for_relative_path() {
        // Whatever the host separator is, the rendered string is forward-slash.
        // Constructing the PathBuf component-by-component keeps the test
        // independent of which platform runs it.
        let p: std::path::PathBuf = ["crates", "alpha", "Cargo.toml"].iter().collect();
        assert_eq!(display_path(&p), "crates/alpha/Cargo.toml");
    }

    #[test]
    fn display_path_replaces_backslashes() {
        // Simulate a path that already carries backslashes (as `Path::display()`
        // would produce on Windows) — the helper must rewrite them.
        let p = std::path::PathBuf::from(r"crates\alpha\Cargo.toml");
        let rendered = display_path(&p);
        // On Windows this gets normalized; on Unix the backslash is treated as
        // a literal character in the single component, so `display()` already
        // yields the input verbatim — either way, no `\` should appear in the
        // rendered output on platforms where `\` is the separator.
        if std::path::MAIN_SEPARATOR == '\\' {
            assert_eq!(rendered, "crates/alpha/Cargo.toml");
        }
    }

    #[test]
    fn parse_unknown_returns_helpful_error() {
        let err = Format::parse("xml").unwrap_err();
        assert!(err.contains("unknown"));
        assert!(err.contains("xml"));
        assert!(err.contains("human"));
    }

    #[test]
    fn render_counts_deny_diagnostics() {
        use crate::diagnostic::{Level, builder::at_workspace};
        let diags = vec![
            at_workspace("workspace-lint::a", "x").build(),
            at_workspace("workspace-lint::b", "x")
                .level(Level::Deny)
                .build(),
            at_workspace("workspace-lint::c", "x")
                .level(Level::Deny)
                .build(),
        ];
        let mut buf = Vec::new();
        let denied = render(Format::Human, &diags, &mut buf).unwrap();
        assert_eq!(denied, 2);
    }
}
