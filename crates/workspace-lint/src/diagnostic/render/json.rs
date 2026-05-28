//! Rustc-compatible JSON renderer. One JSON object per line, no envelope.
//!
//! Field names mirror `rustc`'s `Diagnostic` so that rust-analyzer's
//! `check.overrideCommand` consumes our output as-is and surfaces the
//! `suggested_replacement` as an "Apply suggestion" code action.

use std::io::{self, Write};

use serde::Serialize;

use super::display_path;
use crate::diagnostic::{Applicability, Diagnostic, Level, SilenceAnchor, Span, Suggestion};

#[derive(Serialize)]
struct OutDiagnostic {
    level: &'static str,
    message: String,
    code: OutCode,
    spans: Vec<OutSpan>,
    children: Vec<OutChild>,
    rendered: Option<String>,
}

#[derive(Serialize)]
struct OutCode {
    code: String,
    explanation: Option<String>,
}

#[derive(Serialize)]
struct OutSpan {
    file_name: String,
    byte_start: u32,
    byte_end: u32,
    line_start: u32,
    line_end: u32,
    column_start: u32,
    column_end: u32,
    is_primary: bool,
    label: Option<String>,
    suggested_replacement: Option<String>,
    suggestion_applicability: Option<&'static str>,
}

#[derive(Serialize)]
struct OutChild {
    level: &'static str,
    message: String,
    spans: Vec<OutSpan>,
}

pub(crate) fn write(diagnostics: &[Diagnostic], out: &mut dyn Write) -> io::Result<()> {
    for d in diagnostics {
        let out_d = to_out(d);
        let line = serde_json::to_string(&out_d).expect("serialize Diagnostic");
        writeln!(out, "{line}")?;
    }
    Ok(())
}

fn to_out(d: &Diagnostic) -> OutDiagnostic {
    let mut spans = Vec::new();
    if let Some(p) = &d.primary {
        spans.push(span_to_out(p, true, None, None));
    } else if let Some(fallback) = fallback_span(&d.silence_anchor) {
        spans.push(span_to_out(&fallback, true, None, None));
    }

    let mut children = Vec::new();
    for s in &d.suggestions {
        children.push(suggestion_to_child(s));
    }
    if let Some(silence) = d.silence_suggestion() {
        children.push(suggestion_to_child(&silence));
    }
    for help in &d.helps {
        children.push(OutChild {
            level: "help",
            message: help.clone(),
            spans: Vec::new(),
        });
    }
    for note in &d.notes {
        children.push(OutChild {
            level: "note",
            message: note.clone(),
            spans: Vec::new(),
        });
    }

    OutDiagnostic {
        level: level_str(d.level),
        message: d.message.clone(),
        code: OutCode {
            code: d.lint.to_string(),
            explanation: None,
        },
        spans,
        children,
        rendered: None,
    }
}

fn level_str(l: Level) -> &'static str {
    match l {
        Level::Warn => "warning",
        Level::Deny => "error",
    }
}

fn fallback_span(anchor: &SilenceAnchor) -> Option<Span> {
    match anchor {
        SilenceAnchor::Line { file, line } => Some(Span {
            file: file.clone(),
            line_start: *line,
            line_end: *line,
            col_start: 1,
            col_end: 1,
            byte_start: 0,
            byte_end: 0,
        }),
        SilenceAnchor::File { file } => Some(Span::file_anchor(file.clone())),
        SilenceAnchor::Crate { manifest_dir } => {
            Some(Span::file_anchor(manifest_dir.join("Cargo.toml")))
        }
        SilenceAnchor::Workspace => None,
    }
}

fn span_to_out(
    s: &Span,
    is_primary: bool,
    suggested_replacement: Option<String>,
    applicability: Option<Applicability>,
) -> OutSpan {
    OutSpan {
        file_name: display_path(&s.file),
        byte_start: s.byte_start,
        byte_end: s.byte_end,
        line_start: s.line_start,
        line_end: s.line_end,
        column_start: s.col_start,
        column_end: s.col_end,
        is_primary,
        label: None,
        suggested_replacement,
        suggestion_applicability: applicability.map(|a| a.as_str()),
    }
}

fn suggestion_to_child(s: &Suggestion) -> OutChild {
    OutChild {
        level: "help",
        message: s.message.clone(),
        spans: vec![span_to_out(
            &s.span,
            true,
            Some(s.replacement.clone()),
            Some(s.applicability),
        )],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostic::builder::{at_file, at_workspace};

    fn render_one(d: &Diagnostic) -> serde_json::Value {
        let mut buf = Vec::new();
        write(std::slice::from_ref(d), &mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        serde_json::from_str(s.trim()).unwrap()
    }

    #[test]
    fn json_has_rustc_compatible_top_level_fields() {
        let d = at_file("workspace-lint::file-size", "msg", "src/lib.rs").build();
        let v = render_one(&d);
        assert_eq!(v["level"], "warning");
        assert_eq!(v["message"], "msg");
        assert_eq!(v["code"]["code"], "workspace-lint::file-size");
        assert!(v["spans"].is_array());
        assert!(v["children"].is_array());
    }

    #[test]
    fn json_warn_to_warning_deny_to_error() {
        let warn = at_workspace("workspace-lint::x", "x").build();
        let deny = at_workspace("workspace-lint::x", "x")
            .level(Level::Deny)
            .build();
        assert_eq!(render_one(&warn)["level"], "warning");
        assert_eq!(render_one(&deny)["level"], "error");
    }

    #[test]
    fn json_silence_suggestion_appears_as_help_child_with_replacement() {
        let d = at_file("workspace-lint::file-size", "msg", "src/lib.rs").build();
        let v = render_one(&d);
        let children = v["children"].as_array().unwrap();
        let silence = children
            .iter()
            .find(|c| c["message"].as_str().unwrap().contains("silence with"))
            .expect("silence child present");
        assert_eq!(silence["level"], "help");
        let span = &silence["spans"][0];
        assert_eq!(
            span["suggested_replacement"],
            "workspace_lint::expect!(file_size);\n"
        );
        assert_eq!(span["suggestion_applicability"], "MachineApplicable");
        assert_eq!(span["file_name"], "src/lib.rs");
    }

    #[test]
    fn json_workspace_anchor_has_no_primary_span() {
        let d = at_workspace("workspace-lint::centralized-deps", "msg").build();
        let v = render_one(&d);
        let spans = v["spans"].as_array().unwrap();
        assert!(spans.is_empty());
    }

    #[test]
    fn json_one_object_per_line() {
        let diags = vec![
            at_workspace("workspace-lint::a", "x").build(),
            at_workspace("workspace-lint::b", "x").build(),
        ];
        let mut buf = Vec::new();
        write(&diags, &mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = s.trim().lines().collect();
        assert_eq!(lines.len(), 2);
        for line in lines {
            let _v: serde_json::Value = serde_json::from_str(line).expect("each line is JSON");
        }
    }
}
