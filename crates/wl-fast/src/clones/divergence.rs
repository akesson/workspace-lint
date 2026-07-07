//! Literal-divergence analysis of a clone group: what the concrete literals
//! that detection abstracted away say about the group.
//!
//! Instances of one group share a normalized token stream, so their captured
//! literal sequences align position-for-position. Reading the values
//! column-major (one *value-tuple* per position, across all instances)
//! yields two opposite signals:
//!
//! - **Extraction cost.** Divergent positions whose tuples are identical
//!   co-vary — they are one parameter of the fn an extraction would produce.
//!   The number of distinct tuples ([`Divergence::params`]) is that fn's
//!   parameter count. Zero means the instances are literal-identical (the
//!   most actionable finding); many *independent* parameters means the
//!   "clone" is a data table (match arms mapping the same variants to
//!   different values) — normal code, not copy-paste, and what the lint's
//!   `max-parameters` gate suppresses.
//! - **Drift.** A single position defecting from an otherwise consistent
//!   mapping is the classic copy-paste-bug signature (CP-Miner, OSDI'04;
//!   Juergens, ICSE'09: about half of inconsistent edits to clones are
//!   unintentional). Those are reported as [`Violation`]s and must survive
//!   the table gate — suppressing high-divergence groups wholesale would
//!   silence exactly the bug class.
//!
//! A violation claim is expensive when wrong, so the rule is guarded three
//! ways (see `violations` below): the defect must be one-of-a-kind (a class of
//! support 1 — repeated defects are systematic, not drift), the defecting
//! value must be a *stale twin* (the value the violated mapping holds in
//! some other instance — "edited two copies, forgot this one"), and
//! sentinel values (`0`, `1`, `""`) never anchor a violation in either
//! role. All-distinct tuples — free-text messages, prose — never qualify.

use std::collections::HashMap;
use std::path::PathBuf;

use super::capture::{Lit, LiteralTables};
use super::{CloneGroup, ScanFile};

/// What the concrete literals of one clone group reveal. Produced by
/// [`DivergenceAnalyzer::analyze`].
#[derive(Debug)]
pub struct Divergence {
    /// Total literal positions in the shared normalized stream.
    pub positions: usize,
    /// Positions where not all instances hold the same value.
    pub divergent: usize,
    /// Co-variance classes among the divergent positions — the parameter
    /// count of the fn an extraction would produce. `0` means the instances
    /// are literal-identical.
    pub params: usize,
    /// Probable copy-paste drift, ordered by position. Empty for clean
    /// parameterizations and for data tables.
    pub violations: Vec<Violation>,
}

/// One probable copy-paste drift: a literal breaking an otherwise consistent
/// cross-instance mapping.
#[derive(Debug)]
pub struct Violation {
    /// File of the defecting instance.
    pub file: PathBuf,
    /// Line of the defecting literal (1-based).
    pub line: u32,
    /// The literal found at the defecting cell, verbatim.
    pub found: String,
    /// The value the violated mapping holds for that instance elsewhere.
    pub expected: String,
}

/// Computes [`Divergence`] per group, sharing the lazily-built per-file
/// literal tables across groups. Only meaningful when detection ran with
/// `ignore_literals` — with exact literals, instances are literal-identical
/// by construction.
pub struct DivergenceAnalyzer<'a> {
    tables: LiteralTables<'a>,
}

impl<'a> DivergenceAnalyzer<'a> {
    /// `files` must be the same scan set the groups were found in.
    pub fn new(files: &'a [ScanFile]) -> Self {
        Self {
            tables: LiteralTables::new(files),
        }
    }

    /// Analyze one group. `None` when the captured sequences don't align
    /// (an instance's file missing from the scan set, or a kind-sequence
    /// mismatch) — both defensive: the fingerprint embeds the literal kind
    /// sequence, so aligned capture is the invariant, and a caller getting
    /// `None` should simply skip divergence shaping for that group.
    pub fn analyze(&mut self, group: &CloneGroup) -> Option<Divergence> {
        let slices: Vec<Vec<Lit>> = group
            .instances
            .iter()
            .map(|r| self.tables.slice(r))
            .collect::<Option<_>>()?;
        let first = &slices[0];
        let aligned = slices[1..]
            .iter()
            .all(|s| s.len() == first.len() && s.iter().zip(first).all(|(a, b)| a.kind == b.kind));
        if !aligned {
            return None;
        }

        // Column-major: one value-tuple per position; identical tuples
        // co-vary into one class. ALL positions class up — the all-equal
        // classes never count toward `params`, but they are exactly where
        // the "forgot one copy" drift hides (the forgotten position agrees
        // across instances, so it is invisible to a divergent-only view).
        // `classes` keeps first-seen order so the violation scan below is
        // deterministic.
        let mut class_of: HashMap<Vec<&str>, usize> = HashMap::new();
        let mut classes: Vec<Class<'_>> = Vec::new();
        for p in 0..first.len() {
            let tuple: Vec<&str> = slices.iter().map(|s| s[p].text.as_str()).collect();
            let idx = *class_of.entry(tuple.clone()).or_insert_with(|| {
                let divergent = !tuple.iter().all(|v| *v == tuple[0]);
                classes.push(Class {
                    tuple,
                    positions: Vec::new(),
                    divergent,
                });
                classes.len() - 1
            });
            classes[idx].positions.push(p);
        }

        let divergent = classes
            .iter()
            .filter(|c| c.divergent)
            .map(|c| c.positions.len())
            .sum();
        let params = classes.iter().filter(|c| c.divergent).count();
        let violations = violations(&slices, group, &classes);
        Some(Divergence {
            positions: first.len(),
            divergent,
            params,
            violations,
        })
    }
}

/// One co-variance class: every position sharing one value-tuple.
struct Class<'a> {
    tuple: Vec<&'a str>,
    positions: Vec<usize>,
    divergent: bool,
}

/// The guarded drift rule. A candidate defector is any class of support 1 —
/// including an all-equal one: "renamed `alpha` → `beta` in the copy but
/// forgot one occurrence" leaves a position where the instances *agree*, so
/// the forgotten copy is invisible to a divergent-only view. (A repeated
/// defect is its own consistent mapping — systematic, not drift.) The
/// defector becomes a violation iff some *divergent* class with support ≥2
/// matches its tuple in all instances but exactly one — the defector cell —
/// and the guards hold:
/// - **stale twin**: the found value equals the donor class's value in some
///   *other* instance, the "forgot one copy" signature; all-distinct values
///   are prose, not drift. Donors being divergent-only is what keeps the
///   classic one-position parameterized clone (a lone `(X, Z)` against
///   identity classes) from ever being called a bug;
/// - **sentinels**: neither the found nor the expected value is `0`, `1`,
///   or `""` — too common to carry mapping identity.
///
/// Ambiguity resolves deterministically: the donor is the highest-support
/// class (ties: first seen), and violations are ordered by (file, line).
fn violations(slices: &[Vec<Lit>], group: &CloneGroup, classes: &[Class<'_>]) -> Vec<Violation> {
    let mut out = Vec::new();
    for defector in classes.iter().filter(|c| c.positions.len() == 1) {
        let p = defector.positions[0];
        let donor = classes
            .iter()
            .filter(|c| c.divergent && c.positions.len() >= 2)
            .filter(|c| diff_count(&c.tuple, &defector.tuple) == 1)
            .max_by_key(|c| c.positions.len());
        let Some(donor) = donor else {
            continue;
        };
        let i = donor
            .tuple
            .iter()
            .zip(&defector.tuple)
            .position(|(a, b)| a != b)
            .expect("donor differs in exactly one instance");
        let (found, expected) = (defector.tuple[i], donor.tuple[i]);
        let stale_twin = donor
            .tuple
            .iter()
            .enumerate()
            .any(|(j, v)| j != i && *v == found);
        if !stale_twin || is_sentinel(found) || is_sentinel(expected) {
            continue;
        }
        out.push(Violation {
            file: group.instances[i].file.clone(),
            line: slices[i][p].pos.line as u32,
            found: found.to_string(),
            expected: expected.to_string(),
        });
    }
    out.sort_by_key(|v| (v.file.clone(), v.line));
    out
}

/// Positions (instances) where two equal-length tuples disagree.
fn diff_count(a: &[&str], b: &[&str]) -> usize {
    a.iter().zip(b).filter(|(x, y)| x != y).count()
}

/// `0`, `1`, and `""` in any spelling (`0usize`, `1.0`, `0_u8`) are too
/// common to anchor a violation. Radix literals (`0x10`) are conservatively
/// NOT sentinels — misjudging toward "not a sentinel" only feeds the other
/// guards, never hides real drift.
fn is_sentinel(text: &str) -> bool {
    if text == "\"\"" {
        return true;
    }
    let digits: String = text.chars().filter(|c| *c != '_').collect();
    let num: String = digits
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let suffix = &digits[num.len()..];
    if num.is_empty() || suffix.starts_with(['x', 'o', 'b', 'e', 'E']) {
        return false;
    }
    matches!(num.parse::<f64>(), Ok(v) if v == 0.0 || v == 1.0)
}
