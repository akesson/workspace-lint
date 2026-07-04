use super::*;
use crate::config::GlobPattern;

use std::path::PathBuf;
use tempfile::TempDir;

#[test]
fn glob_set_returns_none_for_empty() {
    assert!(ir::build_glob_set(&[]).is_none());
}

#[test]
fn glob_set_matches_canonical_path_patterns() {
    let set = ir::build_glob_set(&[GlobPattern::from("*Error")]).unwrap();
    assert!(set.is_match("MyError"));
    assert!(!set.is_match("Thing"));
}

// --- the deletion path ---

use crate::lints::unused_pub::ir;
use wl_diagnostic::PubVerdict;

fn ir_span(lo: u32, hi: u32) -> wl_engine::wl_ir::Span {
    wl_engine::wl_ir::Span {
        file: "src/lib.rs".into(),
        lo,
        hi,
        line: 1,
        from_expansion: false,
    }
}

#[test]
fn ir_delete_suggestion_unavailable_when_file_missing() {
    let missing = PathBuf::from("/nonexistent/definitely/not/here.rs");
    assert!(matches!(
        ir::delete_suggestion(&missing, &ir_span(0, 10)),
        ir::DeleteOutcome::Unavailable
    ));
}

#[test]
fn ir_delete_suggestion_unavailable_when_start_ge_end() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("lib.rs");
    std::fs::write(&file, "pub fn gone() {}\n").unwrap();
    assert!(matches!(
        ir::delete_suggestion(&file, &ir_span(10, 10)),
        ir::DeleteOutcome::Unavailable
    ));
}

#[test]
fn ir_delete_suggestion_skips_untracked_file_and_eats_trailing_newline() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("lib.rs");
    std::fs::write(&file, "pub fn gone() {}\nrest").unwrap();
    match ir::delete_suggestion(&file, &ir_span(0, 16)) {
        ir::DeleteOutcome::Skip(s, reason) => {
            assert_eq!(s.span.byte_start, 0);
            assert_eq!(s.span.byte_end, 17, "deletion eats the trailing newline");
            assert_eq!(s.original.as_deref(), Some("pub fn gone() {}"));
            assert!(reason.contains("untracked or has uncommitted changes"));
        }
        other => panic!(
            "expected Skip for an untracked file, got {}",
            ir_outcome(&other)
        ),
    }
}

fn ir_outcome(o: &ir::DeleteOutcome) -> &'static str {
    match o {
        ir::DeleteOutcome::Apply(_) => "Apply",
        ir::DeleteOutcome::Skip(..) => "Skip",
        ir::DeleteOutcome::Unavailable => "Unavailable",
    }
}

#[test]
fn ir_pick_deletion_fix_gates() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("lib.rs");
    std::fs::write(&file, "pub fn gone() {}\n").unwrap();
    // No auto_delete opt-in → tighten fallback.
    assert!(ir::pick_deletion_fix(false, &file, &ir_span(0, 16), PubVerdict::Unused).is_none());
    // IntraCrate never deletes.
    assert!(ir::pick_deletion_fix(true, &file, &ir_span(0, 16), PubVerdict::IntraCrate).is_none());
    // Unused + opt-in on an untracked file → the Skip suggestion (with note).
    let (sugg, note) =
        ir::pick_deletion_fix(true, &file, &ir_span(0, 16), PubVerdict::Unused).expect("Some");
    assert_eq!(
        sugg.applicability,
        wl_diagnostic::Applicability::MaybeIncorrect
    );
    assert!(note.is_some());
}

#[test]
fn kind_filter_ir_vocabulary_is_total() {
    let all = [
        (KindFilter::Function, "fn"),
        (KindFilter::Struct, "struct"),
        (KindFilter::Enum, "enum"),
        (KindFilter::Union, "union"),
        (KindFilter::Trait, "trait"),
        (KindFilter::Type, "type"),
        (KindFilter::Const, "const"),
        (KindFilter::Static, "static"),
        (KindFilter::Module, "mod"),
        (KindFilter::Macro, "macro"),
    ];
    for (filter, expected) in all {
        assert_eq!(filter.to_ir_kind(), expected);
    }
}
