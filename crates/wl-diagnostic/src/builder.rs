//! Ergonomic constructors that each check uses to build a [`Diagnostic`].
//!
//! Checks should reach for the helper that matches their grain (workspace,
//! crate, file, line) so the resulting [`SilenceAnchor`] is correct.

use std::borrow::Cow;
use std::path::PathBuf;

use super::{Diagnostic, Level, Note, SilenceAnchor, Span};

pub struct DiagnosticBuilder {
    lint: Cow<'static, str>,
    level: Level,
    message: String,
    primary: Option<Span>,
    helps: Vec<String>,
    notes: Vec<Note>,
    suggestions: Vec<super::Suggestion>,
    silence_anchor: SilenceAnchor,
    level_is_explicit: bool,
}

impl DiagnosticBuilder {
    pub(crate) fn new(
        lint: &'static str,
        message: impl Into<String>,
        anchor: SilenceAnchor,
    ) -> Self {
        Self {
            lint: Cow::Borrowed(lint),
            level: Level::Warn,
            message: message.into(),
            primary: None,
            helps: Vec::new(),
            notes: Vec::new(),
            suggestions: Vec::new(),
            silence_anchor: anchor,
            level_is_explicit: false,
        }
    }

    // Test-only: production builds diagnostics at the default `Warn` and lets
    // `apply_lint_levels` rewrite the level; only tests set it at construction.
    // (`level_explicit` is the production path for lint-chosen severities.)
    #[allow(dead_code)]
    pub(crate) fn level(mut self, level: Level) -> Self {
        self.level = level;
        self
    }

    /// Set the level *and* mark it as lint-chosen, so the `[lints]` severity
    /// table won't override it. Two uses: a lint with its own per-rule severity
    /// (`architecture`), and a *coverage* note — one saying a finding could not
    /// be judged, which a `deny` must not escalate into a build failure
    /// (`unused-deps`, `orphan-file`).
    pub fn level_explicit(mut self, level: Level) -> Self {
        self.level = level;
        self.level_is_explicit = true;
        self
    }

    pub(crate) fn primary(mut self, span: Span) -> Self {
        self.primary = Some(span);
        self
    }

    pub fn help(mut self, msg: impl Into<String>) -> Self {
        self.helps.push(msg.into());
        self
    }

    /// Per-finding note — renders on every finding that carries it. This is
    /// the right call for anything that explains *this* finding: why a fix is
    /// withheld, what the specific blind spot is, where the duplicate lives.
    pub fn note(mut self, msg: impl Into<String>) -> Self {
        self.notes.push(Note::from(msg.into()));
        self
    }

    /// Lint-level boilerplate note — the identical caveat this lint stamps on
    /// every finding. The human renderer prints it on the lint's first finding
    /// only; structured output still carries it on each.
    pub fn note_once(mut self, msg: impl Into<String>) -> Self {
        self.notes.push(Note {
            text: msg.into(),
            once_per_lint: true,
        });
        self
    }

    pub fn suggestion(mut self, s: super::Suggestion) -> Self {
        self.suggestions.push(s);
        self
    }

    pub fn build(self) -> Diagnostic {
        Diagnostic {
            lint: self.lint,
            level: self.level,
            message: self.message,
            primary: self.primary,
            helps: self.helps,
            notes: self.notes,
            suggestions: self.suggestions,
            silence_anchor: self.silence_anchor,
            level_is_explicit: self.level_is_explicit,
        }
    }
}

/// Build a diagnostic anchored at the workspace root.
pub fn at_workspace(lint: &'static str, message: impl Into<String>) -> DiagnosticBuilder {
    DiagnosticBuilder::new(lint, message, SilenceAnchor::Workspace)
}

/// Build a diagnostic anchored at a single crate.
pub fn at_crate(
    lint: &'static str,
    message: impl Into<String>,
    manifest_dir: impl Into<PathBuf>,
) -> DiagnosticBuilder {
    DiagnosticBuilder::new(
        lint,
        message,
        SilenceAnchor::Crate {
            manifest_dir: manifest_dir.into(),
        },
    )
}

/// Build a diagnostic anchored at a whole file.
pub fn at_file(
    lint: &'static str,
    message: impl Into<String>,
    file: impl Into<PathBuf>,
) -> DiagnosticBuilder {
    let file = file.into();
    DiagnosticBuilder::new(lint, message, SilenceAnchor::File { file: file.clone() })
        .primary(Span::file_anchor(file))
}

/// Build a diagnostic anchored at a single line.
pub fn at_line(
    lint: &'static str,
    message: impl Into<String>,
    file: impl Into<PathBuf>,
    line: u32,
) -> DiagnosticBuilder {
    let file = file.into();
    DiagnosticBuilder::new(
        lint,
        message,
        SilenceAnchor::Line {
            file: file.clone(),
            line,
        },
    )
    .primary(Span {
        file,
        line_start: line,
        line_end: line,
        col_start: 1,
        col_end: 1,
        byte_start: 0,
        byte_end: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Applicability;

    #[test]
    fn at_workspace_has_workspace_anchor_and_no_primary() {
        let d = at_workspace("workspace-lint::centralized-deps", "x").build();
        assert!(matches!(d.silence_anchor, SilenceAnchor::Workspace));
        assert!(d.primary.is_none());
    }

    #[test]
    fn at_crate_sets_crate_anchor() {
        let d = at_crate("workspace-lint::crate-size", "x", "crates/foo").build();
        match d.silence_anchor {
            SilenceAnchor::Crate { manifest_dir } => {
                assert_eq!(manifest_dir, PathBuf::from("crates/foo"))
            }
            _ => panic!("wrong anchor"),
        }
    }

    #[test]
    fn at_file_sets_file_anchor_and_default_primary() {
        let d = at_file("workspace-lint::file-size", "x", "src/lib.rs").build();
        match &d.silence_anchor {
            SilenceAnchor::File { file } => assert_eq!(file, &PathBuf::from("src/lib.rs")),
            _ => panic!("wrong anchor"),
        }
        let p = d.primary.unwrap();
        assert_eq!(p.line_start, 1);
        assert_eq!(p.col_start, 1);
    }

    #[test]
    fn at_line_carries_line_in_anchor_and_primary() {
        let d = at_line("workspace-lint::unused-pub", "x", "src/lib.rs", 12).build();
        match &d.silence_anchor {
            SilenceAnchor::Line { file, line } => {
                assert_eq!(file, &PathBuf::from("src/lib.rs"));
                assert_eq!(*line, 12);
            }
            _ => panic!("wrong anchor"),
        }
        assert_eq!(d.primary.as_ref().unwrap().line_start, 12);
    }

    #[test]
    fn builder_collects_helps_notes_and_suggestions() {
        let s = super::super::Suggestion {
            span: Span::file_anchor("x"),
            message: "m".into(),
            replacement: "r".into(),
            applicability: Applicability::MachineApplicable,
            original: None,
        };
        let d = at_workspace("workspace-lint::centralized-deps", "x")
            .level(Level::Deny)
            .help("hello")
            .note("there")
            .suggestion(s.clone())
            .build();
        assert_eq!(d.level, Level::Deny);
        assert_eq!(d.helps, vec!["hello".to_string()]);
        assert_eq!(d.notes, vec![Note::from("there")]);
        assert!(!d.notes[0].once_per_lint, "note() is per-finding");
        assert_eq!(d.suggestions, vec![s]);
    }
}
