//! The `duplicate-code` baseline: a checked-in record of accepted clone
//! groups so only NEW (or GROWN) duplication fails CI — the SonarQube
//! new-code-gate model, keyed on the portable content fingerprint rather than
//! a line anchor (so it survives reformatting, renames, and moves).
//!
//! Two sides. The **write** side ([`BaselineFile`], built by
//! [`super::collect_baseline`] and serialized by the binary's
//! `--baseline-write`) records exactly the groups the lint currently reports.
//! The **read** side ([`Baseline`], loaded in [`super::DuplicateCode::run`])
//! skips baselined groups and, after the run, reports every entry that no
//! longer matches — a fixed or shrunk clone — so the ratchet can only tighten
//! (PHPStan's `reportUnmatchedIgnoredErrors`). Stale findings carry the lint's
//! own id and normal leveling, so `[lints] duplicate-code` governs whether
//! they fail CI.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use wl_diagnostic::Diagnostic;
use wl_diagnostic::builder::{at_file, at_line};
use wl_engine::fast::clones::CloneGroup;
use wl_lint_api::LintId;

/// The command that regenerates the baseline — named in every remediation.
const REGEN: &str = "workspace-lint --baseline-write";

/// One `[[group]]` entry as read from disk. `fingerprint` + `tokens` are the
/// match key; `instances` is the ratchet; `file` is the anchor named in the
/// stale message. The written-but-advisory `line`/`kind`/`lines` keys are
/// deliberately not fields here — serde ignores them, so they stay pure review
/// context and can never make a hand-edited file fail to load.
#[derive(Deserialize)]
struct RawEntry {
    fingerprint: String,
    tokens: usize,
    instances: usize,
    #[serde(default)]
    file: String,
}

#[derive(Deserialize)]
struct RawFile {
    #[serde(default)]
    group: Vec<RawEntry>,
}

/// A loaded entry plus the mutation state [`Baseline::judge`] records. The
/// `(fingerprint, tokens)` key lives in the index, not here.
#[derive(Debug)]
struct Entry {
    fingerprint: u64,
    /// Recorded instance count (the ratchet floor).
    instances: usize,
    /// Advisory: where the group anchored when the baseline was written.
    file: String,
    /// 1-based line of this entry in the baseline file (the diagnostic anchor).
    src_line: u32,
    /// Set when a firing group matched this entry.
    matched: bool,
    /// The matched group's current instance count (for the overcount message).
    current: usize,
}

/// What [`Baseline::judge`] concluded about one firing group.
pub(crate) enum Verdict {
    /// No entry matches — a new group; emit it normally.
    New,
    /// Baselined at or above the current instance count — skip it.
    Covered,
    /// Matches, but the group has more instances than were accepted — emit it
    /// with a "grew from N" note; `recorded` is the baselined count.
    Grew { recorded: usize },
}

/// The read side: match firing groups against the checked-in entries, then
/// account for entries nothing matched.
#[derive(Debug)]
pub(crate) struct Baseline {
    /// The workspace-relative configured path (the stale-finding anchor).
    rel_path: PathBuf,
    /// `(fingerprint, tokens)` → index into `entries`.
    index: HashMap<(u64, usize), usize>,
    entries: Vec<Entry>,
}

impl Baseline {
    /// Load and index the baseline at `root`/`rel`. A missing file, a parse
    /// error, a malformed fingerprint, or a duplicate key yields a single
    /// diagnostic (anchored at the file) rather than a partial load — the
    /// lint returns just that so the run isn't judged against a broken record.
    /// The error is boxed: `Diagnostic` is large and would bloat every `Ok`.
    pub(crate) fn load(root: &Path, rel: &Path) -> Result<Baseline, Box<Diagnostic>> {
        let full = root.join(rel);
        let text = match fs_err::read_to_string(&full) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(file_error(
                    rel,
                    format!("duplicate-code baseline file `{}` not found", rel.display()),
                    format!(
                        "generate it with `{REGEN}`, or remove `baseline` from [duplicate-code]"
                    ),
                ));
            }
            Err(e) => {
                return Err(file_error(
                    rel,
                    format!(
                        "failed to read duplicate-code baseline `{}`: {e}",
                        rel.display()
                    ),
                    format!("regenerate it with `{REGEN}`"),
                ));
            }
        };
        let raw: RawFile = toml::from_str(&text).map_err(|e| {
            file_error(
                rel,
                format!(
                    "failed to parse duplicate-code baseline `{}`: {}",
                    rel.display(),
                    e.message()
                ),
                format!("regenerate it with `{REGEN}`"),
            )
        })?;

        let mut index = HashMap::new();
        let mut entries = Vec::new();
        for raw in raw.group {
            let Some(fingerprint) = parse_hex(&raw.fingerprint) else {
                return Err(file_error(
                    rel,
                    format!(
                        "duplicate-code baseline `{}` has a malformed fingerprint `{}`",
                        rel.display(),
                        raw.fingerprint
                    ),
                    format!("regenerate it with `{REGEN}`"),
                ));
            };
            let key = (fingerprint, raw.tokens);
            if index.insert(key, entries.len()).is_some() {
                return Err(file_error(
                    rel,
                    format!(
                        "duplicate-code baseline `{}` lists fingerprint {} twice",
                        rel.display(),
                        raw.fingerprint
                    ),
                    format!("regenerate it with `{REGEN}`"),
                ));
            }
            entries.push(Entry {
                fingerprint,
                instances: raw.instances,
                file: raw.file,
                src_line: locate_line(&text, &raw.fingerprint),
                matched: false,
                current: 0,
            });
        }
        Ok(Baseline {
            rel_path: rel.to_path_buf(),
            index,
            entries,
        })
    }

    /// Judge one firing group against the baseline, recording the match.
    pub(crate) fn judge(&mut self, group: &CloneGroup) -> Verdict {
        let Some(&idx) = self.index.get(&(group.fingerprint, group.tokens)) else {
            return Verdict::New;
        };
        let entry = &mut self.entries[idx];
        let current = group.instances.len();
        entry.matched = true;
        entry.current = current;
        if current > entry.instances {
            Verdict::Grew {
                recorded: entry.instances,
            }
        } else {
            Verdict::Covered
        }
    }

    /// Every entry no firing group matched (the clone was fixed) or that
    /// records more instances than remain (the clone shrank) — the ratchet's
    /// obsolete records, in baseline-file order. Consumes the baseline: this
    /// is end-of-run accounting.
    pub(crate) fn stale_findings(self) -> Vec<Diagnostic> {
        self.entries
            .iter()
            .filter_map(|e| {
                if !e.matched {
                    Some(self.orphan_finding(e))
                } else if e.current < e.instances {
                    Some(self.overcount_finding(e))
                } else {
                    None
                }
            })
            .collect()
    }

    fn orphan_finding(&self, e: &Entry) -> Diagnostic {
        at_line(
            LintId::DuplicateCode.id(),
            format!(
                "stale duplicate-code baseline entry: no clone group matches fingerprint {:016x} (was {}, {} instances)",
                e.fingerprint,
                if e.file.is_empty() { "?" } else { &e.file },
                e.instances,
            ),
            self.rel_path.clone(),
            e.src_line,
        )
        .note("the duplication was resolved, or the code changed enough to re-fingerprint")
        .help(format!("regenerate with `{REGEN}` (or delete this entry)"))
        .build()
    }

    fn overcount_finding(&self, e: &Entry) -> Diagnostic {
        at_line(
            LintId::DuplicateCode.id(),
            format!(
                "duplicate-code baseline entry {:016x} records {} instances but only {} remain",
                e.fingerprint, e.instances, e.current,
            ),
            self.rel_path.clone(),
            e.src_line,
        )
        .help(format!("ratchet down: regenerate with `{REGEN}`"))
        .build()
    }
}

/// The write side: the accepted clone groups as they will be serialized.
pub struct BaselineFile {
    entries: Vec<WriteEntry>,
}

struct WriteEntry {
    fingerprint: u64,
    tokens: usize,
    instances: usize,
    file: String,
    line: u32,
    kind: &'static str,
    lines: u32,
}

impl BaselineFile {
    /// Build the write model from the groups the lint currently reports. The
    /// advisory fields come from each group's anchor instance; `file` is the
    /// forward-slashed display path so the file is byte-identical regardless
    /// of platform.
    pub(crate) fn from_groups<'a>(
        groups: impl IntoIterator<Item = &'a CloneGroup>,
    ) -> BaselineFile {
        let mut entries: Vec<WriteEntry> = groups
            .into_iter()
            .map(|g| {
                let anchor = &g.instances[0];
                WriteEntry {
                    fingerprint: g.fingerprint,
                    tokens: g.tokens,
                    instances: g.instances.len(),
                    file: wl_diagnostic::render::display_path(&anchor.file),
                    line: anchor.line_start,
                    kind: anchor.kind.label(),
                    lines: anchor.line_end - anchor.line_start + 1,
                }
            })
            .collect();
        // Deterministic, human-diff-friendly order independent of scan order.
        entries.sort_by(|a, b| {
            (&a.file, a.line, a.fingerprint).cmp(&(&b.file, b.line, b.fingerprint))
        });
        BaselineFile { entries }
    }

    /// How many groups the file records (the `--baseline-write` summary line).
    pub fn groups(&self) -> usize {
        self.entries.len()
    }

    /// Render the baseline as TOML: a header comment naming the regen command,
    /// then one `[[group]]` table per accepted clone.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(
            "# duplicate-code baseline: clone groups accepted as-is. workspace-lint skips\n\
             # these; a group that grows, and any entry no longer matching, is reported.\n\
             # Regenerate with: workspace-lint --baseline-write\n\
             # Match key: fingerprint + tokens. The file/line/kind/lines fields are review\n\
             # context only and may go stale harmlessly.\n",
        );
        for e in &self.entries {
            out.push_str(&format!(
                "\n[[group]]\n\
                 fingerprint = \"{:016x}\"\n\
                 tokens = {}\n\
                 instances = {}\n\
                 file = \"{}\"\n\
                 line = {}\n\
                 kind = \"{}\"\n\
                 lines = {}\n",
                e.fingerprint, e.tokens, e.instances, e.file, e.line, e.kind, e.lines,
            ));
        }
        out
    }
}

/// Parse a 16-hex-digit fingerprint (an optional `0x` prefix is tolerated).
fn parse_hex(s: &str) -> Option<u64> {
    let s = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    u64::from_str_radix(s, 16).ok()
}

/// 1-based line of the entry whose `fingerprint = "<hex>"` line contains
/// `hex`. Fingerprints are unique (duplicates are rejected at load), so the
/// first match is the entry; falls back to line 1 if the text was reshaped.
fn locate_line(text: &str, hex: &str) -> u32 {
    for (i, line) in text.lines().enumerate() {
        if line.contains("fingerprint") && line.contains(hex) {
            return i as u32 + 1;
        }
    }
    1
}

/// One diagnostic anchored at the whole baseline file (missing / unparsable /
/// malformed cases, where no single entry line is meaningful). Boxed to match
/// [`Baseline::load`]'s error type (`Diagnostic` is large).
fn file_error(rel: &Path, message: String, help: String) -> Box<Diagnostic> {
    Box::new(
        at_file(LintId::DuplicateCode.id(), message, rel.to_path_buf())
            .help(help)
            .build(),
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use wl_engine::fast::clones::{CandidateKind, Region};

    use super::*;

    fn group(fingerprint: u64, tokens: usize, instances: usize) -> CloneGroup {
        CloneGroup {
            instances: (0..instances)
                .map(|i| Region {
                    file: PathBuf::from("crates/foo/src/lib.rs"),
                    krate: "foo".into(),
                    line_start: 10 + i as u32,
                    line_end: 20 + i as u32,
                    col_start: 0,
                    col_end: 1,
                    kind: CandidateKind::Fn,
                })
                .collect(),
            tokens,
            fingerprint,
        }
    }

    fn load(text: &str) -> Result<Baseline, Box<Diagnostic>> {
        // `load` reads from disk; drive the parser via a tempdir.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.toml"), text).unwrap();
        Baseline::load(dir.path(), Path::new("b.toml"))
    }

    #[test]
    fn render_then_load_round_trips() {
        let file = BaselineFile::from_groups([&group(0x1234, 50, 3), &group(0xabcd, 60, 2)]);
        assert_eq!(file.groups(), 2);
        let mut base = load(&file.render()).expect("generated baseline must parse");
        // Both groups are covered exactly → skipped, no stale findings.
        assert!(matches!(
            base.judge(&group(0x1234, 50, 3)),
            Verdict::Covered
        ));
        assert!(matches!(
            base.judge(&group(0xabcd, 60, 2)),
            Verdict::Covered
        ));
        assert!(base.stale_findings().is_empty());
    }

    #[test]
    fn judge_new_grew_covered_overcount() {
        let file = BaselineFile::from_groups([&group(0x1234, 50, 2)]);
        let mut base = load(&file.render()).unwrap();
        // An unlisted group is new.
        assert!(matches!(base.judge(&group(0x9999, 50, 2)), Verdict::New));
        // The listed group with more instances grew.
        match base.judge(&group(0x1234, 50, 3)) {
            Verdict::Grew { recorded } => assert_eq!(recorded, 2),
            _ => panic!("expected Grew"),
        }
    }

    #[test]
    fn matched_exactly_leaves_no_stale_finding() {
        let file = BaselineFile::from_groups([&group(0x1234, 50, 2)]);
        let mut base = load(&file.render()).unwrap();
        assert!(matches!(
            base.judge(&group(0x1234, 50, 2)),
            Verdict::Covered
        ));
        assert!(base.stale_findings().is_empty());
    }

    #[test]
    fn unmatched_entry_is_stale() {
        let file = BaselineFile::from_groups([&group(0x1234, 50, 2)]);
        let base = load(&file.render()).unwrap();
        // Nothing judged → the entry is orphaned.
        let stale = base.stale_findings();
        assert_eq!(stale.len(), 1);
        assert!(
            stale[0]
                .message
                .contains("stale duplicate-code baseline entry")
        );
        assert!(stale[0].message.contains("0000000000001234"));
    }

    #[test]
    fn shrunk_group_reports_overcount() {
        let file = BaselineFile::from_groups([&group(0x1234, 50, 3)]);
        let mut base = load(&file.render()).unwrap();
        // Only 2 instances remain of the 3 recorded → covered but over-counted.
        assert!(matches!(
            base.judge(&group(0x1234, 50, 2)),
            Verdict::Covered
        ));
        let stale = base.stale_findings();
        assert_eq!(stale.len(), 1);
        assert!(
            stale[0]
                .message
                .contains("records 3 instances but only 2 remain")
        );
    }

    #[test]
    fn duplicate_fingerprint_is_rejected() {
        let text = "\
            [[group]]\nfingerprint = \"1234\"\ntokens = 50\ninstances = 2\n\
            [[group]]\nfingerprint = \"1234\"\ntokens = 50\ninstances = 2\n";
        let err = load(text).expect_err("duplicate key must error");
        assert!(err.message.contains("twice"));
    }

    #[test]
    fn malformed_fingerprint_is_rejected() {
        let text = "[[group]]\nfingerprint = \"zznothex\"\ntokens = 50\ninstances = 2\n";
        let err = load(text).expect_err("bad hex must error");
        assert!(err.message.contains("malformed fingerprint"));
    }

    #[test]
    fn missing_file_reports_remediation() {
        let dir = tempfile::tempdir().unwrap();
        let err = Baseline::load(dir.path(), Path::new("absent.toml")).expect_err("missing file");
        assert!(err.message.contains("not found"));
        assert!(err.helps.iter().any(|h| h.contains("--baseline-write")));
    }

    #[test]
    fn stale_entry_anchors_at_its_line() {
        let file = BaselineFile::from_groups([&group(0x1234, 50, 2)]);
        let rendered = file.render();
        let base = load(&rendered).unwrap();
        let stale = base.stale_findings();
        // The anchor line must be the `fingerprint = "..."` line, not line 1.
        let expected = rendered
            .lines()
            .position(|l| l.contains("fingerprint") && l.contains("0000000000001234"))
            .map(|i| i as u32 + 1)
            .unwrap();
        assert_eq!(stale[0].primary.as_ref().unwrap().line_start, expected);
    }
}
