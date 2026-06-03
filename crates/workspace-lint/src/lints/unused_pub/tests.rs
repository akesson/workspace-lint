use super::*;
use crate::config::GlobPattern;

#[test]
fn kind_filter_maps_to_item_kind() {
    assert_eq!(KindFilter::Function.to_item_kind(), ItemKind::Fn);
    assert_eq!(KindFilter::Type.to_item_kind(), ItemKind::TypeAlias);
    assert_eq!(KindFilter::Module.to_item_kind(), ItemKind::Module);
}

#[test]
fn glob_set_returns_none_for_empty() {
    assert!(build_glob_set(&[]).is_none());
}

#[test]
fn glob_set_matches_canonical_path_patterns() {
    let set = build_glob_set(&[GlobPattern::from("*Error")]).unwrap();
    assert!(set.is_match("MyError"));
    assert!(!set.is_match("Thing"));
}

// --- delete_suggestion ---

use std::path::PathBuf;
use syn_workspace::SourceSpan;
use tempfile::TempDir;

fn span_for(file: PathBuf, byte_start: u32, byte_end: u32) -> SourceSpan {
    SourceSpan {
        file,
        line: 1,
        column: 1,
        byte_range: if byte_start == 0 && byte_end == 0 {
            None
        } else {
            Some(byte_start..byte_end)
        },
    }
}

#[test]
fn delete_suggestion_unavailable_for_synthetic_span() {
    let span = span_for(PathBuf::from("nonexistent.rs"), 0, 0);
    assert!(matches!(
        delete_suggestion(&span),
        DeleteOutcome::Unavailable
    ));
}

#[test]
fn delete_suggestion_unavailable_when_file_missing() {
    let span = span_for(PathBuf::from("/no/such/file.rs"), 1, 10);
    assert!(matches!(
        delete_suggestion(&span),
        DeleteOutcome::Unavailable
    ));
}

#[test]
fn delete_suggestion_unavailable_when_start_ge_end() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("a.rs");
    std::fs::write(&path, "pub fn foo() {}").unwrap();
    let span = span_for(path, 10, 5);
    assert!(matches!(
        delete_suggestion(&span),
        DeleteOutcome::Unavailable
    ));
}

#[test]
fn delete_suggestion_skips_untracked_file() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("a.rs");
    let contents = "pub fn foo() {}\n";
    std::fs::write(&path, contents).unwrap();
    let span = span_for(path.clone(), 0, contents.len() as u32 - 1);
    match delete_suggestion(&span) {
        DeleteOutcome::Skip(s, reason) => {
            assert_eq!(s.span.byte_start, 0);
            // Span should be extended to swallow the trailing newline.
            assert_eq!(s.span.byte_end, contents.len() as u32);
            assert!(reason.contains("untracked") || reason.contains("uncommitted"));
        }
        other => panic!("expected Skip, got {:?}", std::mem::discriminant(&other)),
    }
}

// --- pick_deletion_fix ---
//
// Coverage for these branches doesn't come from the integration tests in
// `tests/cases/`, which spawn the binary as a subprocess (cargo-llvm-cov
// doesn't instrument subprocesses). These in-process tests keep the CRAP
// score for `pick_deletion_fix` and `apply_structural_fix` under the gate.

#[test]
fn pick_deletion_fix_returns_none_when_auto_delete_off() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("a.rs");
    std::fs::write(&path, "pub fn foo() {}\n").unwrap();
    let span = span_for(path, 0, 10);
    assert!(pick_deletion_fix(false, &span, &Usage::Unused).is_none());
}

#[test]
fn pick_deletion_fix_returns_none_for_intra_crate() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("a.rs");
    std::fs::write(&path, "pub fn foo() {}\n").unwrap();
    let span = span_for(path, 0, 10);
    assert!(pick_deletion_fix(true, &span, &Usage::IntraCrate).is_none());
}

#[test]
fn pick_deletion_fix_returns_none_for_cross_crate() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("a.rs");
    std::fs::write(&path, "pub fn foo() {}\n").unwrap();
    let span = span_for(path, 0, 10);
    assert!(pick_deletion_fix(true, &span, &Usage::CrossCrate).is_none());
}

#[test]
fn pick_deletion_fix_returns_none_when_span_is_synthetic() {
    let span = span_for(PathBuf::from("/no/such/file"), 0, 0);
    assert!(pick_deletion_fix(true, &span, &Usage::Unused).is_none());
}

#[test]
fn pick_deletion_fix_returns_skip_for_untracked_file() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("a.rs");
    let contents = "pub fn foo() {}\n";
    std::fs::write(&path, contents).unwrap();
    let span = span_for(path, 0, contents.len() as u32 - 1);
    let (sugg, note) = pick_deletion_fix(true, &span, &Usage::Unused).expect("Some");
    assert!(note.is_some(), "untracked-file note should be present");
    assert_eq!(sugg.span.byte_start, 0);
}

// --- apply_structural_fix ---

fn make_item(name: &str, vis_byte_range: Option<std::ops::Range<u32>>) -> syn_workspace::Item {
    syn_workspace::Item {
        name: name.into(),
        kind: ItemKind::Fn,
        visibility: syn_workspace::Visibility::Public,
        canonical: syn_workspace::ResolvedPath::new([String::from("demo"), String::from(name)]),
        source: Some(SourceSpan {
            file: PathBuf::from("synthetic.rs"),
            line: 1,
            column: 1,
            byte_range: vis_byte_range.clone(),
        }),
        vis_byte_range,
    }
}

#[test]
fn apply_structural_fix_intra_crate_adds_tighten_suggestion() {
    // `pub fn x() {}` — vis range covers the literal `pub` bytes 0..3.
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("a.rs");
    std::fs::write(&path, "pub fn x() {}\n").unwrap();
    let mut item = make_item("x", Some(0..3));
    item.source = Some(SourceSpan {
        file: path.clone(),
        line: 1,
        column: 1,
        byte_range: Some(0..14),
    });
    let span = item.source.clone().unwrap();
    let builder = crate::diagnostic::builder::at_line(LintId::UnusedPub.id(), "msg", path, 1);
    let built = apply_structural_fix(builder, &item, false, &span, &Usage::IntraCrate).build();
    assert_eq!(
        built.suggestions.len(),
        1,
        "tighten suggestion should be attached"
    );
    assert_eq!(built.suggestions[0].replacement, "pub(crate)");
    // IntraCrate = a referrer was found inside the crate, so tightening is
    // safe to auto-apply.
    assert_eq!(
        built.suggestions[0].applicability,
        crate::diagnostic::Applicability::MachineApplicable,
    );
}

#[test]
fn apply_structural_fix_unused_without_auto_delete_falls_back_to_tighten() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("a.rs");
    std::fs::write(&path, "pub fn y() {}\n").unwrap();
    let mut item = make_item("y", Some(0..3));
    item.source = Some(SourceSpan {
        file: path.clone(),
        line: 1,
        column: 1,
        byte_range: Some(0..14),
    });
    let span = item.source.clone().unwrap();
    let builder = crate::diagnostic::builder::at_line(LintId::UnusedPub.id(), "msg", path, 1);
    let built = apply_structural_fix(builder, &item, false, &span, &Usage::Unused).build();
    assert_eq!(built.suggestions.len(), 1);
    assert_eq!(built.suggestions[0].replacement, "pub(crate)");
    // Unused = the resolver found *zero* referrers — a blind spot (FFI exports,
    // macro-only usage, missed re-exports). The tighten is shown but emitted as
    // `MaybeIncorrect` so `--fix` will not auto-apply it.
    assert_eq!(
        built.suggestions[0].applicability,
        crate::diagnostic::Applicability::MaybeIncorrect,
    );
}

#[test]
fn apply_structural_fix_unused_with_auto_delete_emits_deletion() {
    // Untracked tempdir file — `delete_suggestion` returns `Skip`, which
    // carries the deletion suggestion plus an explanatory note.
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("a.rs");
    let contents = "pub fn z() {}\n";
    std::fs::write(&path, contents).unwrap();
    let item = make_item("z", None);
    let span = SourceSpan {
        file: path.clone(),
        line: 1,
        column: 1,
        byte_range: Some(0..(contents.len() as u32 - 1)),
    };
    let builder = crate::diagnostic::builder::at_line(LintId::UnusedPub.id(), "msg", path, 1);
    let built = apply_structural_fix(builder, &item, true, &span, &Usage::Unused).build();
    assert_eq!(built.suggestions.len(), 1);
    assert_eq!(
        built.suggestions[0].replacement, "",
        "deletion suggestion should have empty replacement"
    );
    // A `Skip` outcome carries a caveat note — verify it landed on the
    // diagnostic.
    assert!(
        built
            .notes
            .iter()
            .any(|n| n.contains("untracked") || n.contains("uncommitted")),
        "deletion-skip note should be attached: {:?}",
        built.notes
    );
}

#[test]
fn apply_structural_fix_no_vis_range_emits_no_suggestion() {
    // Item with no vis_byte_range — neither tighten nor delete suggestion
    // can be built, so the diagnostic carries no structural fix.
    let item = make_item("w", None);
    let span = SourceSpan {
        file: PathBuf::from("/nonexistent"),
        line: 1,
        column: 1,
        byte_range: None,
    };
    let builder =
        crate::diagnostic::builder::at_line(LintId::UnusedPub.id(), "msg", "synthetic.rs", 1);
    let built = apply_structural_fix(builder, &item, false, &span, &Usage::IntraCrate).build();
    assert!(built.suggestions.is_empty());
}
