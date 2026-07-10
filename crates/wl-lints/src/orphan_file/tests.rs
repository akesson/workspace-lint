//! Unit tests for the verdict, over plain sets.
//!
//! The lint's `run()` needs a `SemanticModel`, which has no test builder in this
//! tree — that path is covered by the compiling `tests/cases/orphan-file/`
//! fixtures. Everything decidable without a model is decided here, which is why
//! `classify` takes sets rather than reading them off the models itself.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::classify::{Verdict, classify};

fn set(paths: &[&str]) -> HashSet<PathBuf> {
    paths.iter().map(PathBuf::from).collect()
}

#[test]
fn compiled_file_is_live() {
    let reached = set(&["/w/src/lib.rs"]);
    assert_eq!(
        classify(Path::new("/w/src/lib.rs"), &reached, &reached),
        Verdict::Live
    );
}

/// rustc opens `include_str!` targets, macro-generated `mod` files, and the
/// taken `cfg_attr` path arm even when a syntactic walker names none of them.
/// `rustc_reached` alone decides liveness.
#[test]
fn compiled_file_is_live_even_when_source_never_names_it() {
    assert_eq!(
        classify(
            Path::new("/w/src/generated.rs"),
            &set(&["/w/src/generated.rs"]),
            &set(&[]),
        ),
        Verdict::Live
    );
}

/// The `#[cfg(windows)] mod win;` case on a host-only matrix, and the untaken
/// `#[cfg_attr(windows, path = …)]` arm. Named by the source, opened by
/// nothing. Never a delete suggestion.
#[test]
fn declared_but_never_compiled_is_a_coverage_gap() {
    assert_eq!(
        classify(
            Path::new("/w/src/win.rs"),
            &set(&["/w/src/lib.rs"]),
            &set(&["/w/src/lib.rs", "/w/src/win.rs"]),
        ),
        Verdict::CoverageGap
    );
}

/// Invisible to both tiers: the stale file left behind by a rename. This is the
/// only verdict that tells a user to delete source.
#[test]
fn unnamed_and_uncompiled_is_an_orphan() {
    assert_eq!(
        classify(
            Path::new("/w/src/stale.rs"),
            &set(&["/w/src/lib.rs"]),
            &set(&["/w/src/lib.rs"]),
        ),
        Verdict::Orphan
    );
}

/// The safety property, stated directly: for the lint to advise deletion, both
/// tiers must miss the file. Either one naming it downgrades to a coverage gap.
/// Every historical false positive of this lint violated exactly this.
#[test]
fn either_tier_naming_the_file_prevents_a_delete_suggestion() {
    let f = Path::new("/w/src/x.rs");
    let named = set(&["/w/src/x.rs"]);
    let empty = set(&[]);

    assert_ne!(classify(f, &named, &empty), Verdict::Orphan);
    assert_ne!(classify(f, &empty, &named), Verdict::Orphan);
    assert_eq!(classify(f, &empty, &empty), Verdict::Orphan);
}
