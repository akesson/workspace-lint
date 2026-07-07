//! Apply suppression directives to a stream of diagnostics.
//!
//! `SuppressionMap` is built from a `Vec<Directive>` (parsed by
//! [`crate::directives::scan`]) and answers: given a diagnostic, is it
//! suppressed? And which `expect` directives went unmatched (used by the
//! `stale-expect` meta-lint in step 4 / task #13)?

use std::collections::HashSet;
use std::path::Path;

use crate::directives::{Directive, DirectiveKind, DirectiveOrigin};
use wl_diagnostic::{Diagnostic, SilenceAnchor, builder::at_line};
use wl_lint_api::LintId;

pub(crate) const STALE_EXPECT_LINT: &str = LintId::StaleExpect.id();

/// Emit a `workspace-lint::unknown-lint` diagnostic for every directive that
/// names a lint which doesn't exist — a silent no-op otherwise (the directive
/// would just never match, and an `expect` for it would masquerade as stale).
/// Anchored at the directive's own line; deduped by origin so a Cargo.toml
/// comment that fans out to multiple anchors yields a single diagnostic.
pub(crate) fn unknown_lint_diagnostics(directives: &[Directive]) -> Vec<Diagnostic> {
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
pub(crate) const LOOKBACK_FORWARD: u32 = 3;

pub(crate) struct SuppressionMap {
    entries: Vec<Entry>,
}

struct Entry {
    directive: Directive,
    /// Set to `true` the first time a diagnostic matches an `expect`
    /// directive. After the run, any `expect` with `matched == false` —
    /// *and whose lint actually ran this invocation* — becomes a
    /// `stale-expect` diagnostic. A lint skipped by `--fast-only`, disabled
    /// via `allow`, or outside a `check <lint>` run produced nothing to
    /// match, so its expects carry no staleness signal.
    matched: bool,
}

impl SuppressionMap {
    pub(crate) fn from_directives(directives: Vec<Directive>) -> Self {
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

    /// Would any directive suppress this diagnostic? The read-only twin of
    /// [`SuppressionMap::is_suppressed`] — no `matched` side effect — used by
    /// the unused-pub `--fix` cascade to decide, without disturbing
    /// stale-expect accounting, whether a would-be deletion is silenced (so its
    /// target must not seed a removal). The real filtering + `expect`
    /// fulfilment still runs later via [`is_suppressed`](Self::is_suppressed).
    pub(crate) fn would_suppress(&self, d: &Diagnostic) -> bool {
        let lint_short = d.lint_short();
        self.entries.iter().any(|entry| {
            entry.directive.lint == lint_short
                && applies(&entry.directive.anchor, &d.silence_anchor)
        })
    }

    /// Test whether any directive suppresses this diagnostic. Side-effect:
    /// records `matched = true` on every matching `expect` so the
    /// stale-detection pass picks it up.
    pub(crate) fn is_suppressed(&mut self, d: &Diagnostic) -> bool {
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

    /// One diagnostic per source directive *line* whose `expect` didn't match
    /// anything. A single source-level `expect!` may have produced multiple
    /// internal `Entry` rows — a Cargo.toml comment fans out to Line + Crate
    /// anchors, and `expect(a, b)` names several lints — so we group by
    /// `(origin.file, origin.line)` and emit at most one diagnostic per line,
    /// naming every lint that went stale there.
    ///
    /// **Invariant the key relies on**: every `Entry` from one source directive
    /// carries identical `(origin.file, origin.line)`, whatever anchor grains or
    /// lint names the scanner emitted. Adding a new anchor grain stays safe as
    /// long as the fan-out reuses the same `origin`. See
    /// `directives.rs::parses_toml_comment_directive_emits_line_and_crate_anchors`
    /// for the Line + Crate emitter that's the original motivating case.
    ///
    /// `ran` is the set of lints that actually executed this invocation
    /// (registry membership plus the pipeline meta-lints); expects for any
    /// other lint are exempt from staleness — see [`Entry::matched`].
    ///
    /// When *every* lint a directive names is judged stale, the diagnostic also
    /// carries a `MachineApplicable` deletion suggestion (`--fix` removes the
    /// line). If any lint at that line is unknown, didn't run, or matched, the
    /// deletion is withheld — removing the line would also silence a live or
    /// unjudged lint — and the diagnostic stays help-only. `root` is the
    /// workspace root the origin paths are relative to, used to read the file
    /// when building the deletion span.
    pub(crate) fn stale_expects(&self, ran: &HashSet<LintId>, root: &Path) -> Vec<Diagnostic> {
        use std::collections::BTreeMap;
        use std::collections::HashMap;

        /// Per-line accumulation of every `expect` entry's verdict.
        struct OriginGroup<'a> {
            /// Representative origin (widest `line..=line_end` span seen).
            origin: &'a DirectiveOrigin,
            /// Judgeable lint (known + ran) → did any of its anchors match?
            judgeable: BTreeMap<String, bool>,
            /// Any lint at this line we can't judge (unknown, or didn't run
            /// this invocation). Its presence blocks the whole-line deletion.
            has_unjudgeable: bool,
        }

        let mut by_line: HashMap<(std::path::PathBuf, u32), OriginGroup> = HashMap::new();
        for entry in &self.entries {
            if entry.directive.kind != DirectiveKind::Expect {
                continue;
            }
            let origin = &entry.directive.origin;
            let group = by_line
                .entry((origin.file.clone(), origin.line))
                .or_insert_with(|| OriginGroup {
                    origin,
                    judgeable: BTreeMap::new(),
                    has_unjudgeable: false,
                });
            // Track the widest span at this line (a pathological same-line
            // `expect!(a); expect!(b);` pair) so the deletion covers all of it.
            if origin.line_end > group.origin.line_end {
                group.origin = origin;
            }
            match LintId::from_short(&entry.directive.lint) {
                // Unknown lint: already reported by `unknown-lint`, and its
                // typo probably wants fixing rather than deleting — never judge
                // it stale, and block the deletion so the line survives.
                None => group.has_unjudgeable = true,
                // Known but not run this invocation (`--fast-only`, an `allow`
                // level, a `check <other-lint>` run): produced nothing an
                // expect could match, so "unmatched" carries no staleness
                // signal here.
                Some(lint) if !ran.contains(&lint) => group.has_unjudgeable = true,
                Some(_) => {
                    *group
                        .judgeable
                        .entry(entry.directive.lint.clone())
                        .or_insert(false) |= entry.matched;
                }
            }
        }

        let mut out: Vec<Diagnostic> = Vec::new();
        for group in by_line.into_values() {
            let stale: Vec<&str> = group
                .judgeable
                .iter()
                .filter(|(_, matched)| !**matched)
                .map(|(lint, _)| lint.as_str())
                .collect();
            if stale.is_empty() {
                continue;
            }
            // Delete only when the whole line is stale: any unjudgeable lint or
            // any still-live lint sharing the line means removing it would
            // silence something we shouldn't.
            let fully_stale =
                !group.has_unjudgeable && group.judgeable.values().all(|matched| !*matched);
            let mut builder = at_line(
                STALE_EXPECT_LINT,
                stale_message(&stale),
                group.origin.file.clone(),
                group.origin.line,
            )
            .help(stale_help(&stale, fully_stale))
            .note("a stale expect usually means the underlying issue has been fixed");
            if fully_stale
                && let Some(sug) = crate::directives::deletion_suggestion(root, group.origin)
            {
                builder = builder.suggestion(sug);
            }
            out.push(builder.build());
        }
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

/// The stale-expect message, listing every lint that went stale at one line.
fn stale_message(stale: &[&str]) -> String {
    match stale {
        [one] => format!("expect directive for `{one}` did not match any diagnostic"),
        many => format!(
            "expect directives for {} did not match any diagnostic",
            backtick_list(many)
        ),
    }
}

/// The stale-expect help line. When the whole directive line is stale it
/// invites deleting it (the fix `--fix` applies); when other lints on the
/// line are still live or unjudged, it names exactly which lint(s) to remove
/// so the reader doesn't delete a line that still does work.
fn stale_help(stale: &[&str], fully_stale: bool) -> String {
    if fully_stale {
        let tracked = match stale {
            [_] => "lint it tracks is",
            _ => "lints it tracks are",
        };
        format!("remove this expect — the {tracked} no longer firing")
    } else {
        format!(
            "remove {} from this expect — the other lints on this line still apply",
            backtick_list(stale)
        )
    }
}

fn backtick_list(lints: &[&str]) -> String {
    lints
        .iter()
        .map(|l| format!("`{l}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `true` if `directive` is at the same scope as `diag`, a wider one
/// containing it, or (for line-level directives) within `LOOKBACK_FORWARD`
/// lines above the diagnostic. Path comparisons are base-insensitive — see
/// [`SilenceAnchor::contains`] / [`SilenceAnchor::same_file`].
fn applies(directive: &SilenceAnchor, diag: &SilenceAnchor) -> bool {
    if directive.contains(diag) {
        return true;
    }
    // A line directive *above* the diagnostic (a comment above a dep line, or
    // the `// workspace-lint: expect(...)` written above an item) still applies
    // within the lookback window. Containment only catches the exact-line case.
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
        && SilenceAnchor::same_file(f_dir, f_diag)
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
    use crate::directives::{DirectiveKind, DirectiveOrigin};
    use std::path::PathBuf;
    use tempfile::TempDir;
    use wl_diagnostic::Applicability;
    use wl_diagnostic::builder::{at_crate, at_file, at_line as build_at_line, at_workspace};

    /// The "every lint ran" set — the staleness domain of a full default run,
    /// which is what most of these tests model.
    fn all_ran() -> HashSet<LintId> {
        LintId::ALL.iter().copied().collect()
    }

    fn allow(lint: &str, anchor: SilenceAnchor) -> Directive {
        Directive {
            kind: DirectiveKind::Allow,
            lint: lint.into(),
            anchor: anchor.clone(),
            origin: DirectiveOrigin {
                file: file_of(&anchor).unwrap_or_else(|| PathBuf::from("dummy")),
                line: 1,
                line_end: 1,
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
                line_end: origin_line,
            },
        }
    }

    /// A dummy workspace root for tests that don't exercise the deletion
    /// suggestion — its origin files don't exist on disk, so
    /// `deletion_suggestion` returns `None` and the diagnostic stays help-only.
    fn no_root() -> &'static Path {
        Path::new("/nonexistent-workspace-root")
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
    fn relative_directive_suppresses_absolute_diagnostic() {
        // The directive scanner yields workspace-relative paths; `unused-pub`
        // anchors with the resolver's absolute paths. They must still match.
        let mut map = SuppressionMap::from_directives(vec![allow(
            "unused-pub",
            SilenceAnchor::Line {
                file: PathBuf::from("crates/demo/src/lib.rs"),
                line: 5,
            },
        )]);
        let d = build_at_line(
            "workspace-lint::unused-pub",
            "x",
            "/abs/wl/crates/demo/src/lib.rs",
            5,
        )
        .build();
        assert!(map.is_suppressed(&d));
    }

    #[test]
    fn relative_comment_directive_above_absolute_item_suppresses() {
        // The `--fix`-written form: a comment directive on line 4 (relative)
        // suppresses the item finding on line 5 (absolute) via lookback.
        let mut map = SuppressionMap::from_directives(vec![allow(
            "unused-pub",
            SilenceAnchor::Line {
                file: PathBuf::from("crates/demo/src/lib.rs"),
                line: 4,
            },
        )]);
        let d = build_at_line(
            "workspace-lint::unused-pub",
            "x",
            "/abs/wl/crates/demo/src/lib.rs",
            5,
        )
        .build();
        assert!(map.is_suppressed(&d));
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
        assert!(map.stale_expects(&all_ran(), no_root()).is_empty());
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
        let stales = map.stale_expects(&all_ran(), no_root());
        assert_eq!(stales.len(), 1);
        assert_eq!(stales[0].lint, STALE_EXPECT_LINT);
        assert!(stales[0].message.contains("file-size"));
        assert_eq!(stales[0].primary.as_ref().unwrap().line_start, 1);
    }

    #[test]
    fn unmatched_expect_for_lint_that_did_not_run_is_not_stale() {
        // The lint never ran this invocation (`--fast-only`, an `allow`
        // level, or a `check <other-lint>` run) — "unmatched" carries no
        // staleness signal, so the expect must not be reported.
        let map = SuppressionMap::from_directives(vec![expect(
            "unused-deps",
            SilenceAnchor::File {
                file: PathBuf::from("src/lib.rs"),
            },
            "src/lib.rs",
            1,
        )]);
        let ran = HashSet::from([LintId::FileSize]);
        assert!(
            map.stale_expects(&ran, no_root()).is_empty(),
            "expect for a lint that did not run must not be judged stale"
        );
        // Same map, lint in the ran set: the staleness verdict returns.
        assert_eq!(map.stale_expects(&all_ran(), no_root()).len(), 1);
    }

    #[test]
    fn unknown_lint_expect_is_not_also_stale() {
        // A typo'd lint name can never match, but it's already reported by
        // `unknown-lint` — it must NOT additionally surface as stale-expect.
        let map = SuppressionMap::from_directives(vec![expect(
            "unusd-deps",
            SilenceAnchor::File {
                file: PathBuf::from("src/lib.rs"),
            },
            "src/lib.rs",
            1,
        )]);
        assert!(
            map.stale_expects(&all_ran(), no_root()).is_empty(),
            "unknown-lint-named expect should not double-report as stale-expect"
        );
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
        assert!(map.stale_expects(&all_ran(), no_root()).is_empty());
    }

    // --- stale-expect deletion suggestion (the `--fix` surface) ---

    fn line_expect(lint: &str, file: &str, line: u32) -> Directive {
        expect(
            lint,
            SilenceAnchor::Line {
                file: PathBuf::from(file),
                line,
            },
            file,
            line,
        )
    }

    /// Apply a suggestion's byte-range deletion to `content` and return the
    /// result — the same replace the `--fix` applier performs.
    fn apply_deletion(content: &str, sug: &wl_diagnostic::Suggestion) -> String {
        let mut fixed = content.to_string();
        fixed.replace_range(sug.span.byte_start as usize..sug.span.byte_end as usize, "");
        fixed
    }

    #[test]
    fn fully_stale_expect_attaches_deletion_suggestion() {
        let tmp = TempDir::new().unwrap();
        let body = "[dependencies]\n# workspace-lint: expect(unused-deps)\nserde = \"1\"\n";
        std::fs::write(tmp.path().join("Cargo.toml"), body).unwrap();
        let map =
            SuppressionMap::from_directives(vec![line_expect("unused-deps", "Cargo.toml", 2)]);
        let stales = map.stale_expects(&all_ran(), tmp.path());
        assert_eq!(stales.len(), 1);
        let sug = stales[0]
            .suggestions
            .first()
            .expect("a deletion suggestion");
        assert_eq!(sug.replacement, "");
        assert_eq!(sug.applicability, Applicability::MachineApplicable);
        assert!(sug.span.byte_end > sug.span.byte_start);
        assert_eq!(apply_deletion(body, sug), "[dependencies]\nserde = \"1\"\n");
    }

    #[test]
    fn deletion_suggestion_is_crlf_safe() {
        let tmp = TempDir::new().unwrap();
        let body = "[dependencies]\r\n# workspace-lint: expect(unused-deps)\r\nserde = \"1\"\r\n";
        std::fs::write(tmp.path().join("Cargo.toml"), body).unwrap();
        let map =
            SuppressionMap::from_directives(vec![line_expect("unused-deps", "Cargo.toml", 2)]);
        let stales = map.stale_expects(&all_ran(), tmp.path());
        let sug = stales[0].suggestions.first().unwrap();
        assert_eq!(
            apply_deletion(body, sug),
            "[dependencies]\r\nserde = \"1\"\r\n"
        );
    }

    #[test]
    fn multi_lint_all_stale_lists_both_and_attaches_suggestion() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "# workspace-lint: expect(unused-deps, centralized-deps)\n",
        )
        .unwrap();
        let map = SuppressionMap::from_directives(vec![
            line_expect("unused-deps", "Cargo.toml", 1),
            line_expect("centralized-deps", "Cargo.toml", 1),
        ]);
        let stales = map.stale_expects(&all_ran(), tmp.path());
        // One diagnostic for the whole line, naming both stale lints.
        assert_eq!(stales.len(), 1);
        assert!(stales[0].message.contains("centralized-deps"));
        assert!(stales[0].message.contains("unused-deps"));
        assert_eq!(stales[0].suggestions.len(), 1);
    }

    #[test]
    fn partially_stale_multi_lint_expect_has_no_suggestion() {
        // One directive line names two lints; one matched (live), one is stale.
        // Deleting the line would silence the live one — so no fix, and the
        // message names only the stale lint.
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "# workspace-lint: expect(unused-deps, centralized-deps)\n",
        )
        .unwrap();
        let mut map = SuppressionMap::from_directives(vec![
            line_expect("unused-deps", "Cargo.toml", 1),
            line_expect("centralized-deps", "Cargo.toml", 1),
        ]);
        // centralized-deps fires on this line → its expect matches (live).
        let d = build_at_line("workspace-lint::centralized-deps", "x", "Cargo.toml", 1).build();
        assert!(map.is_suppressed(&d));
        let stales = map.stale_expects(&all_ran(), tmp.path());
        assert_eq!(stales.len(), 1);
        assert!(stales[0].message.contains("unused-deps"));
        assert!(
            !stales[0].message.contains("centralized-deps"),
            "a still-live lint must not be named as stale"
        );
        assert!(
            stales[0].suggestions.is_empty(),
            "no deletion — it would silence the live lint"
        );
        // The help must direct the reader at the stale lint only — "remove
        // this expect" would invite deleting the live suppression with it.
        let help = &stales[0].helps[0];
        assert!(
            help.contains("remove `unused-deps` from this expect"),
            "partial-stale help must name the lint to remove, got: {help}"
        );
    }

    #[test]
    fn unknown_lint_at_same_line_blocks_deletion() {
        // A typo'd lint sharing the directive line: the known lint is stale,
        // but deleting the line would also drop the typo the user likely wants
        // to fix (unknown-lint points at it). Report the stale lint, no fix.
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "# workspace-lint: expect(unused-deps, unusd-typo)\n",
        )
        .unwrap();
        let map = SuppressionMap::from_directives(vec![
            line_expect("unused-deps", "Cargo.toml", 1),
            line_expect("unusd-typo", "Cargo.toml", 1),
        ]);
        let stales = map.stale_expects(&all_ran(), tmp.path());
        assert_eq!(stales.len(), 1);
        assert!(stales[0].message.contains("unused-deps"));
        assert!(
            stales[0].suggestions.is_empty(),
            "an unknown lint on the line blocks the whole-line deletion"
        );
        assert!(
            stales[0].helps[0].contains("remove `unused-deps` from this expect"),
            "help must not invite whole-line deletion while the typo'd lint is on it"
        );
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
