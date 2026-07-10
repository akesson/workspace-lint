//! The verdict: is a `src/**.rs` file compiled, merely *declared*, or dead?
//!
//! Kept as a pure function over plain sets so it is unit-testable. There is no
//! test builder for `SemanticModel` anywhere in the tree, so a judgment written
//! inline in `run()` could only ever be exercised by the compiling
//! `tests/cases` harness. Everything here takes `HashSet<PathBuf>`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// What the two tiers, together, say about one source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// rustc opened it under at least one declared `[engine]` config.
    Live,
    /// The crate's source names it, but no declared config ever opened it —
    /// it is gated behind a `cfg` the config matrix doesn't cover. Reporting
    /// this as dead would be a false positive; reporting the *matrix* as
    /// incomplete is the honest finding.
    CoverageGap,
    /// No config compiled it and nothing in the crate's source names it.
    /// Deleting it cannot change what the workspace compiles.
    Orphan,
}

/// Judge one file.
///
/// The asymmetry between the two sets is deliberate. `rustc_reached` is exact —
/// it is rustc's own record of the files it opened, so `cfg_attr` paths, macro-
/// generated `mod`s, `include!` in any position, and build-script-computed
/// include paths are all already resolved. `declared_reach` is a syntactic
/// over-approximation that only ever *downgrades* a finding.
///
/// So the destructive verdict requires a file to be invisible to **both** tiers,
/// and a gap in either one fails safe: a gap in `rustc_reached` yields a
/// coverage gap (noise), a gap in `declared_reach` is impossible to reach
/// without one in `rustc_reached` too.
pub(crate) fn classify(
    file: &Path,
    rustc_reached: &HashSet<PathBuf>,
    declared_reach: &HashSet<PathBuf>,
) -> Verdict {
    if rustc_reached.contains(file) {
        Verdict::Live
    } else if declared_reach.contains(file) {
        Verdict::CoverageGap
    } else {
        Verdict::Orphan
    }
}
