//! Apply suppression directives to a stream of diagnostics.
//!
//! `SuppressionMap` is built from a `Vec<Directive>` (parsed by
//! [`crate::directives::scan`]) and answers: given a diagnostic, is it
//! suppressed? And which `expect` directives went unmatched (used by the
//! `stale-expect` meta-lint in step 4 / task #13)?

use crate::diagnostic::{Diagnostic, SilenceAnchor, builder::at_line};
use crate::directives::{Directive, DirectiveKind};
use crate::lints::LintId;

pub(crate) const STALE_EXPECT_LINT: &str = LintId::StaleExpect.id();

/// Emit a `workspace-lint::unknown-lint` diagnostic for every directive that
/// names a lint which doesn't exist — a silent no-op otherwise (the directive
/// would just never match, and an `expect` for it would masquerade as stale).
/// Anchored at the directive's own line; deduped by origin so a Cargo.toml
/// comment that fans out to multiple anchors yields a single diagnostic.
pub(crate) fn unknown_lint_diagnostics(directives: &[Directive]) -> Vec<Diagnostic> {
    use std::collections::HashSet;

    let known: Vec<&str> = LintId::ALL.iter().map(|l| l.short()).collect();
    let mut seen: HashSet<(std::path::PathBuf, u32, String)> = HashSet::new();
    let mut out = Vec::new();
    for d in directives {
        if LintId::from_short(&d.lint).is_some() {
            continue;
        }
        if !seen.insert((d.origin.file.clone(), d.origin.line, d.lint.clone())) {
            continue;
        }
        let kind = match d.kind {
            DirectiveKind::Allow => "allow",
            DirectiveKind::Expect => "expect",
        };
        let mut builder = at_line(
            LintId::UnknownLint.id(),
            format!("unknown lint `{}` in `{kind}` directive", d.lint),
            d.origin.file.clone(),
            d.origin.line,
        );
        if let Some(sugg) = crate::suggest::closest(&d.lint, &known) {
            builder = builder.help(format!("did you mean `{sugg}`?"));
        }
        out.push(builder.build());
    }
    out
}

/// Lookback window (in lines) when matching a TOML/Markdown comment
/// directive to a diagnostic on a nearby line. A directive on line 5 will
/// suppress a diagnostic on lines 5, 6, 7, or 8.
const LOOKBACK_FORWARD: u32 = 3;

pub(crate) struct SuppressionMap {
    entries: Vec<Entry>,
}

struct Entry {
    directive: Directive,
    /// Set to `true` the first time a diagnostic matches an `expect`
    /// directive. After the run, any `expect` with `matched == false`
    /// becomes a `stale-expect` diagnostic.
    matched: bool,
}

impl SuppressionMap {
    pub fn from_directives(directives: Vec<Directive>) -> Self {
        Self {
            entries: directives
                .into_iter()
                .map(|d| Entry {
                    directive: d,
                    matched: false,
                })
                .collect(),
        }
    }

    /// Test whether any directive suppresses this diagnostic. Side-effect:
    /// records `matched = true` on every matching `expect` so the
    /// stale-detection pass picks it up.
    pub fn is_suppressed(&mut self, d: &Diagnostic) -> bool {
        let lint_short = d.lint_short();
        let mut suppressed = false;
        for entry in &mut self.entries {
            if entry.directive.lint != lint_short {
                continue;
            }
            if !applies(&entry.directive.anchor, &d.silence_anchor) {
                continue;
            }
            suppressed = true;
            if entry.directive.kind == DirectiveKind::Expect {
                entry.matched = true;
            }
        }
        suppressed
    }

    /// One diagnostic per *origin* (file + line + lint) whose `expect`
    /// directive didn't match anything. A single source-level `expect!` may
    /// have produced multiple internal `Entry` rows (e.g. a Cargo.toml
    /// comment fans out to Line + Crate anchors); we group by origin so the
    /// user sees a single stale diagnostic per source directive.
    ///
    /// **Invariant the key relies on**: every `Entry` that originated from
    /// the same source directive carries identical
    /// `(directive.origin.file, directive.origin.line, directive.lint)`,
    /// regardless of which anchor grains the scanner emitted. Adding a new
    /// anchor grain (e.g. a File-and-Line pair for non-Rust files) is
    /// safe — the dedup keeps working as long as the fan-out reuses the
    /// same `origin`. See `directives.rs::cargo_toml_fan_out` for the
    /// Line + Crate emitter that's the original motivating case.
    pub fn stale_expects(&self) -> Vec<Diagnostic> {
        use std::collections::HashMap;

        // origin_key -> (any_matched, representative_entry)
        let mut by_origin: HashMap<(std::path::PathBuf, u32, String), (bool, &Entry)> =
            HashMap::new();
        for entry in &self.entries {
            if entry.directive.kind != DirectiveKind::Expect {
                continue;
            }
            let key = (
                entry.directive.origin.file.clone(),
                entry.directive.origin.line,
                entry.directive.lint.clone(),
            );
            by_origin
                .entry(key)
                .and_modify(|(any, _)| *any |= entry.matched)
                .or_insert((entry.matched, entry));
        }

        let mut out: Vec<Diagnostic> = by_origin
            .into_values()
            .filter(|(matched, _)| !*matched)
            .map(|(_, e)| {
                let file = e.directive.origin.file.clone();
                let line = e.directive.origin.line;
                at_line(
                    STALE_EXPECT_LINT,
                    format!(
                        "expect directive for `{}` did not match any diagnostic",
                        e.directive.lint
                    ),
                    file,
                    line,
                )
                .help("remove this expect — the lint it tracks is no longer firing")
                .note("a stale expect usually means the underlying issue has been fixed")
                .build()
            })
            .collect();
        // Stable order so renderers / tests don't flap on HashMap iteration.
        out.sort_by(|a, b| {
            let a_span = a.primary.as_ref();
            let b_span = b.primary.as_ref();
            match (a_span, b_span) {
                (Some(a), Some(b)) => {
                    (a.file.clone(), a.line_start).cmp(&(b.file.clone(), b.line_start))
                }
                _ => std::cmp::Ordering::Equal,
            }
        });
        out
    }
}

/// `true` if `directive` is at the same scope as `diag`, a wider one
/// containing it, or (for line-level TOML directives) within
/// `LOOKBACK_FORWARD` lines above the diagnostic.
fn applies(directive: &SilenceAnchor, diag: &SilenceAnchor) -> bool {
    if directive.contains(diag) {
        return true;
    }
    // Special case for TOML/Markdown: a comment directive *above* a dep
    // line should still apply. Containment math doesn't capture that, so
    // explicitly handle the line-near-line case.
    if let (
        SilenceAnchor::Line {
            file: f_dir,
            line: l_dir,
        },
        SilenceAnchor::Line {
            file: f_diag,
            line: l_diag,
        },
    ) = (directive, diag)
        && f_dir == f_diag
        && *l_diag >= *l_dir
        && *l_diag - *l_dir <= LOOKBACK_FORWARD
    {
        return true;
    }
    false
}

/// Filter a vector in place, retaining only the diagnostics that are not
/// suppressed by the map. Returns the count of suppressed entries.
pub(crate) fn apply(map: &mut SuppressionMap, diagnostics: &mut Vec<Diagnostic>) -> usize {
    let before = diagnostics.len();
    diagnostics.retain(|d| !map.is_suppressed(d));
    before - diagnostics.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostic::builder::{at_crate, at_file, at_line as build_at_line, at_workspace};
    use crate::directives::{DirectiveKind, DirectiveOrigin};
    use std::path::PathBuf;

    fn allow(lint: &str, anchor: SilenceAnchor) -> Directive {
        Directive {
            kind: DirectiveKind::Allow,
            lint: lint.into(),
            anchor: anchor.clone(),
            origin: DirectiveOrigin {
                file: file_of(&anchor).unwrap_or_else(|| PathBuf::from("dummy")),
                line: 1,
            },
        }
    }

    fn expect(lint: &str, anchor: SilenceAnchor, origin_file: &str, origin_line: u32) -> Directive {
        Directive {
            kind: DirectiveKind::Expect,
            lint: lint.into(),
            anchor,
            origin: DirectiveOrigin {
                file: PathBuf::from(origin_file),
                line: origin_line,
            },
        }
    }

    fn file_of(a: &SilenceAnchor) -> Option<PathBuf> {
        match a {
            SilenceAnchor::Line { file, .. } | SilenceAnchor::File { file } => Some(file.clone()),
            SilenceAnchor::Crate { manifest_dir } => Some(manifest_dir.clone()),
            SilenceAnchor::Workspace => None,
        }
    }

    // --- unknown_lint_diagnostics ---

    #[test]
    fn unknown_lint_diagnostics_flags_unknown_and_suggests() {
        let dirs = vec![
            // Unknown near-miss → one diagnostic with a "did you mean".
            expect(
                "unused-dep",
                SilenceAnchor::File {
                    file: PathBuf::from("a.rs"),
                },
                "a.rs",
                3,
            ),
            // A real lint produces nothing.
            allow(
                "file-size",
                SilenceAnchor::File {
                    file: PathBuf::from("b.rs"),
                },
            ),
        ];
        let out = unknown_lint_diagnostics(&dirs);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].lint, LintId::UnknownLint.id());
        assert!(out[0].message.contains("unused-dep"), "{}", out[0].message);
        assert!(
            out[0].helps.iter().any(|h| h.contains("unused-deps")),
            "expected a suggestion, got {:?}",
            out[0].helps
        );
    }

    #[test]
    fn unknown_lint_diagnostics_dedup_by_origin() {
        // The Cargo.toml fan-out emits two directives for one comment (a Line
        // and a Crate anchor) sharing an origin; they collapse to one finding.
        let dirs = vec![
            expect(
                "nope",
                SilenceAnchor::File {
                    file: PathBuf::from("Cargo.toml"),
                },
                "Cargo.toml",
                2,
            ),
            expect(
                "nope",
                SilenceAnchor::Crate {
                    manifest_dir: PathBuf::from("."),
                },
                "Cargo.toml",
                2,
            ),
        ];
        assert_eq!(unknown_lint_diagnostics(&dirs).len(), 1);
    }

    // --- exact-scope match ---

    #[test]
    fn allow_at_same_file_suppresses_diagnostic() {
        let mut map = SuppressionMap::from_directives(vec![allow(
            "file-size",
            SilenceAnchor::File {
                file: PathBuf::from("src/lib.rs"),
            },
        )]);
        let d = at_file("workspace-lint::file-size", "x", "src/lib.rs").build();
        assert!(map.is_suppressed(&d));
    }

    #[test]
    fn allow_for_different_file_does_not_suppress() {
        let mut map = SuppressionMap::from_directives(vec![allow(
            "file-size",
            SilenceAnchor::File {
                file: PathBuf::from("src/other.rs"),
            },
        )]);
        let d = at_file("workspace-lint::file-size", "x", "src/lib.rs").build();
        assert!(!map.is_suppressed(&d));
    }

    #[test]
    fn allow_for_different_lint_does_not_suppress() {
        let mut map = SuppressionMap::from_directives(vec![allow(
            "unused-pub",
            SilenceAnchor::File {
                file: PathBuf::from("src/lib.rs"),
            },
        )]);
        let d = at_file("workspace-lint::file-size", "x", "src/lib.rs").build();
        assert!(!map.is_suppressed(&d));
    }

    // --- containment (wider scope catches narrower) ---

    #[test]
    fn workspace_allow_suppresses_diagnostic_anywhere() {
        let mut map =
            SuppressionMap::from_directives(vec![allow("file-size", SilenceAnchor::Workspace)]);
        let d = at_file("workspace-lint::file-size", "x", "src/foo.rs").build();
        assert!(map.is_suppressed(&d));
    }

    #[test]
    fn crate_allow_suppresses_diagnostic_inside_crate() {
        let mut map = SuppressionMap::from_directives(vec![allow(
            "file-size",
            SilenceAnchor::Crate {
                manifest_dir: PathBuf::from("crates/foo"),
            },
        )]);
        let d = at_file("workspace-lint::file-size", "x", "crates/foo/src/lib.rs").build();
        assert!(map.is_suppressed(&d));
    }

    #[test]
    fn crate_allow_does_not_suppress_diagnostic_in_other_crate() {
        let mut map = SuppressionMap::from_directives(vec![allow(
            "file-size",
            SilenceAnchor::Crate {
                manifest_dir: PathBuf::from("crates/foo"),
            },
        )]);
        let d = at_file("workspace-lint::file-size", "x", "crates/bar/src/lib.rs").build();
        assert!(!map.is_suppressed(&d));
    }

    #[test]
    fn file_allow_suppresses_line_diagnostic_in_same_file() {
        let mut map = SuppressionMap::from_directives(vec![allow(
            "unused-pub",
            SilenceAnchor::File {
                file: PathBuf::from("src/lib.rs"),
            },
        )]);
        let d = build_at_line("workspace-lint::unused-pub", "x", "src/lib.rs", 42).build();
        assert!(map.is_suppressed(&d));
    }

    // --- TOML lookback ---

    #[test]
    fn toml_directive_above_dep_line_suppresses() {
        // Directive on line 4, diagnostic on line 5 (1 line below): match.
        let directive_anchor = SilenceAnchor::Line {
            file: PathBuf::from("Cargo.toml"),
            line: 4,
        };
        let mut map = SuppressionMap::from_directives(vec![allow("unused-deps", directive_anchor)]);
        let d = build_at_line("workspace-lint::unused-deps", "x", "Cargo.toml", 5).build();
        assert!(map.is_suppressed(&d));
    }

    #[test]
    fn toml_directive_three_lines_above_still_matches() {
        let directive_anchor = SilenceAnchor::Line {
            file: PathBuf::from("Cargo.toml"),
            line: 4,
        };
        let mut map = SuppressionMap::from_directives(vec![allow("unused-deps", directive_anchor)]);
        let d = build_at_line("workspace-lint::unused-deps", "x", "Cargo.toml", 7).build();
        assert!(map.is_suppressed(&d));
    }

    #[test]
    fn toml_directive_far_above_does_not_match() {
        let directive_anchor = SilenceAnchor::Line {
            file: PathBuf::from("Cargo.toml"),
            line: 4,
        };
        let mut map = SuppressionMap::from_directives(vec![allow("unused-deps", directive_anchor)]);
        let d = build_at_line("workspace-lint::unused-deps", "x", "Cargo.toml", 20).build();
        assert!(!map.is_suppressed(&d));
    }

    #[test]
    fn toml_directive_below_diagnostic_does_not_match() {
        let directive_anchor = SilenceAnchor::Line {
            file: PathBuf::from("Cargo.toml"),
            line: 10,
        };
        let mut map = SuppressionMap::from_directives(vec![allow("unused-deps", directive_anchor)]);
        let d = build_at_line("workspace-lint::unused-deps", "x", "Cargo.toml", 5).build();
        assert!(!map.is_suppressed(&d));
    }

    // --- expect fulfilment + staleness ---

    #[test]
    fn expect_directive_suppresses_matching_diagnostic_and_is_not_stale() {
        let mut map = SuppressionMap::from_directives(vec![expect(
            "file-size",
            SilenceAnchor::File {
                file: PathBuf::from("src/lib.rs"),
            },
            "src/lib.rs",
            1,
        )]);
        let d = at_file("workspace-lint::file-size", "x", "src/lib.rs").build();
        assert!(map.is_suppressed(&d));
        assert!(map.stale_expects().is_empty());
    }

    #[test]
    fn unmatched_expect_becomes_stale_diagnostic() {
        let map = SuppressionMap::from_directives(vec![expect(
            "file-size",
            SilenceAnchor::File {
                file: PathBuf::from("src/lib.rs"),
            },
            "src/lib.rs",
            1,
        )]);
        // No matching diagnostic was ever passed through `is_suppressed`.
        let stales = map.stale_expects();
        assert_eq!(stales.len(), 1);
        assert_eq!(stales[0].lint, STALE_EXPECT_LINT);
        assert!(stales[0].message.contains("file-size"));
        assert_eq!(stales[0].primary.as_ref().unwrap().line_start, 1);
    }

    #[test]
    fn allow_unmatched_is_not_stale() {
        let map = SuppressionMap::from_directives(vec![allow(
            "file-size",
            SilenceAnchor::File {
                file: PathBuf::from("src/lib.rs"),
            },
        )]);
        // `allow` never goes stale; only `expect` does.
        assert!(map.stale_expects().is_empty());
    }

    // --- `apply` integration ---

    #[test]
    fn apply_removes_suppressed_and_keeps_others() {
        let mut map =
            SuppressionMap::from_directives(vec![allow("file-size", SilenceAnchor::Workspace)]);
        let mut diagnostics = vec![
            at_file("workspace-lint::file-size", "a", "x.rs").build(),
            at_file("workspace-lint::unused-pub", "b", "x.rs").build(),
            at_file("workspace-lint::file-size", "c", "y.rs").build(),
        ];
        let removed = apply(&mut map, &mut diagnostics);
        assert_eq!(removed, 2);
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].lint.contains("unused-pub"));
    }

    #[test]
    fn workspace_anchor_diagnostic_suppressed_only_by_workspace_allow() {
        let d = at_workspace("workspace-lint::centralized-deps", "x").build();
        let mut wide = SuppressionMap::from_directives(vec![allow(
            "centralized-deps",
            SilenceAnchor::Workspace,
        )]);
        let mut narrow = SuppressionMap::from_directives(vec![allow(
            "centralized-deps",
            SilenceAnchor::File {
                file: PathBuf::from("crates/foo/src/lib.rs"),
            },
        )]);
        assert!(wide.is_suppressed(&d));
        assert!(!narrow.is_suppressed(&d));
    }

    #[test]
    fn crate_anchor_diagnostic_suppressed_by_crate_allow_in_same_crate() {
        let d = at_crate("workspace-lint::crate-size", "x", "crates/foo").build();
        let mut map = SuppressionMap::from_directives(vec![allow(
            "crate-size",
            SilenceAnchor::Crate {
                manifest_dir: PathBuf::from("crates/foo"),
            },
        )]);
        assert!(map.is_suppressed(&d));
    }
}
