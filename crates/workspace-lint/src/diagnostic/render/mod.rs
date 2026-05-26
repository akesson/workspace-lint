//! Renderers for [`Diagnostic`]s.
//!
//! Three formats:
//! - [`human`]: clippy-style text with diff-style suggestions.
//! - [`json`]: rustc-compatible JSON, one object per line.
//! - [`github`]: GitHub Actions workflow commands for PR annotations.

use std::io::{self, Write};

use super::Diagnostic;

pub mod github;
pub mod human;
pub mod json;

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
