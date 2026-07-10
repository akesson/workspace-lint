//! Structured diagnostic type that every workspace-lint check emits.
//!
//! Shape mirrors `rustc`'s `DiagnosticSpan` closely enough that
//! `--message-format=json` output is consumable by rust-analyzer's
//! `check.overrideCommand` without an adapter.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

pub mod builder;
pub mod render;

/// Stable identifier for a single check. Format: `workspace-lint::<kebab-name>`.
/// Severity. Drives both the displayed `warning:`/`error:` prefix and the
/// process exit code (a single `Deny`-level diagnostic flips exit to 1).
///
/// Deserializable from `"warn"`/`"deny"` so config's `[lints]` table can
/// override per-diagnostic levels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Warn,
    Deny,
}

impl Level {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Level::Warn => "warning",
            Level::Deny => "error",
        }
    }
}

/// Confidence in a suggested fix. Mirrors `rustc::lint::Applicability`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Applicability {
    MachineApplicable,
    MaybeIncorrect,
    // We never emit these two, but keep the enum a 1:1 mirror of
    // `rustc::lint::Applicability` (all four are handled by `as_str`).
    #[allow(dead_code)]
    HasPlaceholders,
    #[allow(dead_code)]
    Unspecified,
}

impl Applicability {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Applicability::MachineApplicable => "MachineApplicable",
            Applicability::MaybeIncorrect => "MaybeIncorrect",
            Applicability::HasPlaceholders => "HasPlaceholders",
            Applicability::Unspecified => "Unspecified",
        }
    }
}

/// One `= note:` line on a finding.
///
/// A note is per-finding signal by default: it renders on every finding that
/// carries it. `once_per_lint` marks shared boilerplate — the same caveat a
/// lint stamps on all of its findings — which the human renderer prints on
/// the first finding only. Structured output (`json`/`github`) always carries
/// every note; the collapse is a terminal-readability concern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub text: String,
    pub once_per_lint: bool,
}

impl<T: Into<String>> From<T> for Note {
    /// A bare string is a per-finding note — the default, never collapsed.
    fn from(text: T) -> Self {
        Note {
            text: text.into(),
            once_per_lint: false,
        }
    }
}

/// Pointer at a region of source. Carries everything needed by the three
/// renderers and by `--fix`'s byte-range applier.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Span {
    pub file: PathBuf,
    pub line_start: u32,
    pub line_end: u32,
    pub col_start: u32,
    pub col_end: u32,
    pub byte_start: u32,
    pub byte_end: u32,
}

impl Span {
    /// A degenerate "the whole file" span at line 1 col 1 with zero length —
    /// used by checks whose grain is the file itself (file-size) and by the
    /// rendered "if intentional, silence with:" hint that targets the top of
    /// the file.
    pub(crate) fn file_anchor(file: impl Into<PathBuf>) -> Self {
        Self {
            file: file.into(),
            line_start: 1,
            line_end: 1,
            col_start: 1,
            col_end: 1,
            byte_start: 0,
            byte_end: 0,
        }
    }
}

/// Concrete suggested rewrite. `MachineApplicable` ones drive `--fix`.
///
/// Everything here is *structural* by contract — replacements, deletions, and
/// pure insertions (a zero-width span, e.g. the centralized-deps
/// `[workspace.dependencies]` seed) alike. Silence directives must never be
/// pushed as a `Suggestion`: renderers synthesize them from the
/// [`SilenceAnchor`] (`Diagnostic::silence_suggestion`), which is what keeps
/// "`--fix` never writes a silence directive" a structural property of this
/// list rather than a filter over it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    pub span: Span,
    pub message: String,
    pub replacement: String,
    pub applicability: Applicability,
    /// The exact source bytes the span covers, captured at build time so the
    /// human renderer can show the real `-` line in a deletion/replacement diff
    /// (clippy-style) instead of a `…` / `<existing>` placeholder. `None` for
    /// pure insertions and for the rare site that can't read its source; the
    /// renderer falls back to the placeholder then. Renderers other than
    /// `human` ignore it, and `--fix` never reads it.
    pub original: Option<String>,
}

impl Suggestion {
    /// `true` for an empty-replacement suggestion — a deletion. The `--fix`
    /// applier keys its union-vs-refuse overlap policy on exactly this.
    pub fn is_deletion(&self) -> bool {
        self.replacement.is_empty()
    }
}

/// The engine's usage verdict for an `unused-pub` finding — selects the fix
/// shape. (`CrossCrate` items never produce a fix, so they're absent here.)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PubVerdict {
    /// Zero referrers found under any config → the fix deletes the item
    /// (under `--fix-auto-delete`; tighten otherwise).
    Unused,
    /// Only same-crate referrers found → the fix tightens to `pub`.
    IntraCrate,
    /// Referrers exist but every one is test code (any crate) — production-dead.
    /// No fix is machine-applied: narrowing trips `dead_code` on the plain
    /// build, and deletion would orphan the referencing tests (the cascade's
    /// scaffolding rule is the one path that may delete these).
    TestOnly,
}

/// What "silencing this lint" means for a given diagnostic — the unit the
/// suppression map (and the rendered "if intentional, silence with:" hint)
/// operates on.
///
/// `Line ⊂ File ⊂ Crate ⊂ Workspace`: a directive at a wider scope silences
/// every diagnostic at a narrower one inside it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SilenceAnchor {
    Line { file: PathBuf, line: u32 },
    File { file: PathBuf },
    Crate { manifest_dir: PathBuf },
    Workspace,
}

impl SilenceAnchor {
    /// `true` if `other` is the same anchor or a strictly narrower one
    /// contained inside `self`. Used by the suppression map to decide
    /// whether a directive applies to a diagnostic.
    ///
    /// File comparisons are **path-base-insensitive** ([`SilenceAnchor::same_file`]):
    /// the directive scanner produces workspace-relative paths, but some lints
    /// (notably `unused-pub`) anchor with the resolver's *absolute* paths, so a
    /// directive on `crates/x/src/lib.rs` must still contain a diagnostic on
    /// `/abs/…/crates/x/src/lib.rs`.
    pub fn contains(&self, other: &SilenceAnchor) -> bool {
        match (self, other) {
            (SilenceAnchor::Workspace, _) => true,
            (SilenceAnchor::Crate { manifest_dir }, SilenceAnchor::Crate { manifest_dir: o }) => {
                Self::same_file(manifest_dir, o)
            }
            (SilenceAnchor::Crate { manifest_dir }, SilenceAnchor::File { file })
            | (SilenceAnchor::Crate { manifest_dir }, SilenceAnchor::Line { file, .. }) => {
                file.starts_with(manifest_dir)
            }
            (SilenceAnchor::File { file }, SilenceAnchor::File { file: o }) => {
                Self::same_file(file, o)
            }
            (SilenceAnchor::File { file }, SilenceAnchor::Line { file: o, .. }) => {
                Self::same_file(file, o)
            }
            (
                SilenceAnchor::Line { file, line },
                SilenceAnchor::Line {
                    file: o_file,
                    line: o_line,
                },
            ) => Self::same_file(file, o_file) && line == o_line,
            _ => false,
        }
    }

    /// Two anchor file paths name the same file despite possibly differing
    /// bases: equal, or one is a whole-component suffix of the other (a
    /// workspace-relative path vs the resolver's absolute one).
    pub fn same_file(a: &Path, b: &Path) -> bool {
        a == b || a.ends_with(b) || b.ends_with(a)
    }

    /// Returns the file the anchor refers to, if any. Used by renderers that
    /// need a `file:line:col` even for `Crate`/`Workspace` anchors (falls back
    /// to the manifest path).
    pub fn file(&self) -> Option<&Path> {
        match self {
            SilenceAnchor::Line { file, .. } | SilenceAnchor::File { file } => Some(file),
            SilenceAnchor::Crate { manifest_dir } => Some(manifest_dir),
            SilenceAnchor::Workspace => None,
        }
    }
}

/// One emitted finding. Every check produces these; renderers consume them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub lint: Cow<'static, str>,
    pub level: Level,
    pub message: String,
    pub primary: Option<Span>,
    pub helps: Vec<String>,
    pub notes: Vec<Note>,
    pub suggestions: Vec<Suggestion>,
    pub silence_anchor: SilenceAnchor,
    /// `true` when the lint itself chose this diagnostic's level (e.g. an
    /// `architecture` rule with an explicit `severity`). The `[lints]` table's
    /// `apply_lint_levels` pass leaves these untouched, so a per-rule severity
    /// isn't silently clobbered by a blanket `[lints] <lint> = …` override.
    pub level_is_explicit: bool,
    /// `true` when the crate owning this diagnostic's anchor declares the
    /// `workspace-lint-marker` dependency, so the `workspace_lint::expect!`
    /// macro actually compiles there. Selects the silence-hint form for
    /// `.rs` anchors: macro when available, the `// workspace-lint:
    /// expect(…)` comment (which needs no dependency) otherwise. Set by a
    /// post-collection pass in the binary (it has the `FastModel`); the
    /// builder default is `false` — hinting a macro that won't compile is
    /// the strictly worse failure.
    pub marker_available: bool,
}

impl Diagnostic {
    /// Withhold every `MachineApplicable` *deletion* this diagnostic carries —
    /// downgrade it to `MaybeIncorrect` so `--fix` counts it as withheld —
    /// pushing `note` (the reason the user reads) iff something actually
    /// changed. Returns whether it did. Rewrites are left alone: a veto is
    /// always about the deletion.
    pub fn withhold_deletions(&mut self, note: &str) -> bool {
        let mut changed = false;
        for s in &mut self.suggestions {
            if s.applicability == Applicability::MachineApplicable && s.is_deletion() {
                s.applicability = Applicability::MaybeIncorrect;
                changed = true;
            }
        }
        if changed {
            self.notes.push(note.into());
        }
        changed
    }

    /// Snake-case form of the lint name suitable for the marker macro
    /// (`workspace_lint::allow!(<this>)`). Strips the `workspace-lint::`
    /// prefix and converts dashes to underscores.
    pub(crate) fn lint_ident(&self) -> String {
        self.lint
            .strip_prefix("workspace-lint::")
            .unwrap_or(&self.lint)
            .replace('-', "_")
    }

    /// Kebab-case form suitable for the comment directive
    /// (`# workspace-lint: allow(<this>)`).
    pub fn lint_short(&self) -> &str {
        self.lint
            .strip_prefix("workspace-lint::")
            .unwrap_or(&self.lint)
    }

    /// Build the rendered "if intentional, silence with:" hint that every
    /// silencable diagnostic carries. The suggestion text is picked from
    /// the anchor's file extension: `.rs` files get the macro form,
    /// everything else gets the comment form. Renderers attach this to the
    /// human/JSON/github output so an author can paste it manually;
    /// `--fix` deliberately ignores it.
    pub(crate) fn silence_suggestion(&self) -> Option<Suggestion> {
        let (span, is_rust) = match &self.silence_anchor {
            SilenceAnchor::Line { file, line } => {
                let is_rs = file.extension().and_then(|e| e.to_str()) == Some("rs");
                let span = Span {
                    file: file.clone(),
                    line_start: *line,
                    line_end: *line,
                    col_start: 1,
                    col_end: 1,
                    byte_start: 0,
                    byte_end: 0,
                };
                (span, is_rs)
            }
            SilenceAnchor::File { file } => {
                let is_rs = file.extension().and_then(|e| e.to_str()) == Some("rs");
                (Span::file_anchor(file.clone()), is_rs)
            }
            SilenceAnchor::Crate { manifest_dir } => {
                (Span::file_anchor(manifest_dir.join("Cargo.toml")), false)
            }
            SilenceAnchor::Workspace => (Span::file_anchor(PathBuf::from("Cargo.toml")), false),
        };

        // `expect!` is preferred over `allow!` because workspace-lint emits
        // `stale-expect` when the underlying lint stops firing — silences
        // can't quietly rot. `allow!` remains in the marker crate for
        // genuinely permanent silences and for items the lint can't reach
        // (e.g. `unused-pub` items inside `exclude-crates`). The macro form
        // is hinted only when the anchored crate actually depends on the
        // marker crate ([`Diagnostic::marker_available`]); otherwise the
        // dependency-free `//` comment directive — pasting a hint must
        // never be a compile error.
        let replacement = if is_rust && self.marker_available {
            format!("workspace_lint::expect!({});\n", self.lint_ident())
        } else if is_rust {
            format!("// workspace-lint: expect({})\n", self.lint_short())
        } else {
            format!("# workspace-lint: expect({})\n", self.lint_short())
        };

        Some(Suggestion {
            span,
            message: "if intentional, silence with:".into(),
            replacement,
            applicability: Applicability::MachineApplicable,
            original: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> SilenceAnchor {
        SilenceAnchor::Workspace
    }
    fn crate_at(p: &str) -> SilenceAnchor {
        SilenceAnchor::Crate {
            manifest_dir: PathBuf::from(p),
        }
    }
    fn file_at(p: &str) -> SilenceAnchor {
        SilenceAnchor::File {
            file: PathBuf::from(p),
        }
    }
    fn line_at(p: &str, l: u32) -> SilenceAnchor {
        SilenceAnchor::Line {
            file: PathBuf::from(p),
            line: l,
        }
    }

    // --- SilenceAnchor::contains containment lattice ---

    #[test]
    fn workspace_contains_everything() {
        assert!(workspace().contains(&workspace()));
        assert!(workspace().contains(&crate_at("crates/foo")));
        assert!(workspace().contains(&file_at("crates/foo/src/lib.rs")));
        assert!(workspace().contains(&line_at("crates/foo/src/lib.rs", 10)));
    }

    #[test]
    fn crate_contains_its_files_and_lines() {
        let c = crate_at("crates/foo");
        assert!(c.contains(&crate_at("crates/foo")));
        assert!(c.contains(&file_at("crates/foo/src/lib.rs")));
        assert!(c.contains(&line_at("crates/foo/src/lib.rs", 3)));
    }

    #[test]
    fn crate_does_not_contain_other_crates_files() {
        let c = crate_at("crates/foo");
        assert!(!c.contains(&file_at("crates/bar/src/lib.rs")));
        assert!(!c.contains(&line_at("crates/bar/src/lib.rs", 1)));
    }

    #[test]
    fn file_contains_its_lines_only() {
        let f = file_at("src/lib.rs");
        assert!(f.contains(&file_at("src/lib.rs")));
        assert!(f.contains(&line_at("src/lib.rs", 9)));
        assert!(!f.contains(&line_at("src/other.rs", 9)));
        assert!(!f.contains(&file_at("src/other.rs")));
    }

    #[test]
    fn line_contains_exact_match_only() {
        let l = line_at("src/lib.rs", 5);
        assert!(l.contains(&line_at("src/lib.rs", 5)));
        assert!(!l.contains(&line_at("src/lib.rs", 6)));
        assert!(!l.contains(&file_at("src/lib.rs")));
    }

    #[test]
    fn narrower_does_not_contain_wider() {
        assert!(!file_at("a").contains(&crate_at("crates/foo")));
        assert!(!crate_at("a").contains(&workspace()));
    }

    // --- silence_suggestion text ---

    fn diag(anchor: SilenceAnchor) -> Diagnostic {
        Diagnostic {
            lint: Cow::Borrowed("workspace-lint::file-size"),
            level: Level::Warn,
            message: "x".into(),
            primary: None,
            helps: vec![],
            notes: vec![],
            suggestions: vec![],
            silence_anchor: anchor,
            level_is_explicit: false,
            marker_available: false,
        }
    }

    #[test]
    fn silence_for_rust_file_uses_macro_form_only_with_marker() {
        // Default (no marker dep known): the dependency-free comment form —
        // hinting a macro that won't compile is the strictly worse failure.
        let d = diag(SilenceAnchor::File {
            file: PathBuf::from("src/lib.rs"),
        });
        let s = d.silence_suggestion().unwrap();
        assert_eq!(s.replacement, "// workspace-lint: expect(file-size)\n");
        assert_eq!(s.applicability, Applicability::MachineApplicable);

        // With the marker dependency declared, the macro form.
        let mut d = diag(SilenceAnchor::File {
            file: PathBuf::from("src/lib.rs"),
        });
        d.marker_available = true;
        let s = d.silence_suggestion().unwrap();
        assert_eq!(s.replacement, "workspace_lint::expect!(file_size);\n");
    }

    #[test]
    fn silence_for_rust_line_anchors_to_that_line() {
        let d = diag(SilenceAnchor::Line {
            file: PathBuf::from("src/lib.rs"),
            line: 42,
        });
        let s = d.silence_suggestion().unwrap();
        assert_eq!(s.replacement, "// workspace-lint: expect(file-size)\n");
        assert_eq!(s.span.line_start, 42);
    }

    #[test]
    fn silence_for_toml_uses_comment_form() {
        let d = diag(SilenceAnchor::Line {
            file: PathBuf::from("Cargo.toml"),
            line: 7,
        });
        let s = d.silence_suggestion().unwrap();
        assert_eq!(s.replacement, "# workspace-lint: expect(file-size)\n");
    }

    #[test]
    fn silence_for_crate_anchor_targets_cargo_toml() {
        let d = diag(SilenceAnchor::Crate {
            manifest_dir: PathBuf::from("crates/foo"),
        });
        let s = d.silence_suggestion().unwrap();
        assert_eq!(s.span.file, PathBuf::from("crates/foo/Cargo.toml"));
        assert!(s.replacement.starts_with("# workspace-lint: expect"));
    }

    #[test]
    fn silence_for_workspace_anchor_targets_root_cargo_toml() {
        let d = diag(SilenceAnchor::Workspace);
        let s = d.silence_suggestion().unwrap();
        assert_eq!(s.span.file, PathBuf::from("Cargo.toml"));
    }

    // --- lint name helpers ---

    #[test]
    fn lint_ident_strips_prefix_and_kebab_to_snake() {
        let d = diag(SilenceAnchor::Workspace);
        assert_eq!(d.lint_ident(), "file_size");
    }

    #[test]
    fn lint_short_strips_prefix_keeps_kebab() {
        let d = diag(SilenceAnchor::Workspace);
        assert_eq!(d.lint_short(), "file-size");
    }
}
