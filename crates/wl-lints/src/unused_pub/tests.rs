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

use crate::unused_pub::{deletion, ir};
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
        deletion::delete_suggestion(&missing, &ir_span(0, 10)),
        deletion::DeleteOutcome::Unavailable
    ));
}

#[test]
fn ir_delete_suggestion_unavailable_when_start_ge_end() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("lib.rs");
    std::fs::write(&file, "pub fn gone() {}\n").unwrap();
    assert!(matches!(
        deletion::delete_suggestion(&file, &ir_span(10, 10)),
        deletion::DeleteOutcome::Unavailable
    ));
}

#[test]
fn ir_delete_suggestion_skips_untracked_file_and_eats_trailing_newline() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("lib.rs");
    std::fs::write(&file, "pub fn gone() {}\nrest").unwrap();
    match deletion::delete_suggestion(&file, &ir_span(0, 16)) {
        deletion::DeleteOutcome::Skip(s, reason) => {
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

fn ir_outcome(o: &deletion::DeleteOutcome) -> &'static str {
    match o {
        deletion::DeleteOutcome::Apply(_) => "Apply",
        deletion::DeleteOutcome::Skip(..) => "Skip",
        deletion::DeleteOutcome::Unavailable => "Unavailable",
    }
}

// --- deletion surface: preceding attributes (LeaveDates 2026-07-05) ---
// `full_span` can't see cfg-stripped or macro-consumed attributes; the
// deletion extends over them lexically or they'd be orphaned onto the next
// item (syntax error before `}`, silent re-target otherwise).

/// Extension measured from the item-start offset within `src` (the `pub`).
fn extended_start(src: &str) -> usize {
    deletion::extend_over_preceding_attrs(src, src.find("pub fn").unwrap())
}

#[test]
fn attr_extension_single_attr() {
    let src = "#[cfg(test)]\npub fn gone() {}\n";
    assert_eq!(extended_start(src), 0);
}

#[test]
fn attr_extension_stacked_attrs_and_blank_line() {
    let src = "fn keep() {}\n\n#[inline]\n#[cfg(test)]\n\npub fn gone() {}\n";
    assert_eq!(extended_start(src), src.find("#[inline]").unwrap());
}

#[test]
fn attr_extension_multiline_attr() {
    let src = "fn keep() {}\n#[tracing::instrument(\n    level = \"debug\",\n    skip(x),\n)]\npub fn gone() {}\n";
    assert_eq!(extended_start(src), src.find("#[tracing").unwrap());
}

#[test]
fn attr_extension_bracket_inside_string() {
    let src = "fn keep() {}\n#[doc = \"has ] and [ inside\"]\npub fn gone() {}\n";
    assert_eq!(extended_start(src), src.find("#[doc").unwrap());
}

#[test]
fn attr_extension_escaped_quote_inside_string() {
    let src = "fn keep() {}\n#[doc = \"say \\\"]hi\\\"\"]\npub fn gone() {}\n";
    assert_eq!(extended_start(src), src.find("#[doc").unwrap());
}

#[test]
fn attr_extension_never_consumes_inner_attr() {
    let src = "#![allow(dead_code)]\npub fn gone() {}\n";
    assert_eq!(extended_start(src), src.find("pub fn").unwrap());
}

#[test]
fn attr_extension_stops_at_plain_code() {
    let src = "fn keep() {}\npub fn gone() {}\n";
    assert_eq!(extended_start(src), src.find("pub fn").unwrap());
}

#[test]
fn attr_extension_consumes_doc_lines() {
    let src = "fn keep() {}\n/// docs\n#[cfg(test)]\npub fn gone() {}\n";
    assert_eq!(extended_start(src), src.find("/// docs").unwrap());
}

#[test]
fn attr_extension_bails_on_raw_string_fence() {
    // `r#"…"#` breaks the backward bracket math — no extension, attr stays
    // (today's behavior; never a wrong wider deletion).
    let src = "fn keep() {}\n#[doc = r#\"raw ] here\"#]\npub fn gone() {}\n";
    assert_eq!(extended_start(src), src.find("pub fn").unwrap());
}

#[test]
fn blank_line_eating_below_item() {
    let src = "fn a() {}\n\n\nfn b() {}\n";
    // `end` sits after "fn a() {}\n" — the two blank lines are consumed.
    assert_eq!(deletion::eat_blank_lines(src, 10), 12);
    // No blanks → unchanged.
    assert_eq!(deletion::eat_blank_lines("fn a() {}\nfn b() {}\n", 10), 10);
}

#[test]
fn ir_delete_suggestion_includes_attrs_and_blank_lines() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("lib.rs");
    let src = "fn keep() {}\n\n#[cfg(test)]\npub fn gone() {}\n\nfn tail() {}\n";
    std::fs::write(&file, src).unwrap();
    let lo = src.find("pub fn").unwrap() as u32;
    let hi = (src.find("gone() {}").unwrap() + "gone() {}".len()) as u32;
    match deletion::delete_suggestion(&file, &ir_span(lo, hi)) {
        deletion::DeleteOutcome::Skip(s, _) => {
            let mut fixed = src.to_string();
            fixed.replace_range(s.span.byte_start as usize..s.span.byte_end as usize, "");
            assert_eq!(
                fixed, "fn keep() {}\n\nfn tail() {}\n",
                "attr deleted with the item, one blank separator survives"
            );
        }
        other => panic!("expected Skip, got {}", ir_outcome(&other)),
    }
}

#[test]
fn ir_pick_deletion_fix_gates() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("lib.rs");
    std::fs::write(&file, "pub fn gone() {}\n").unwrap();
    // No auto_delete opt-in → tighten fallback.
    assert!(
        deletion::pick_deletion_fix(false, &file, &ir_span(0, 16), PubVerdict::Unused).is_none()
    );
    // IntraCrate never deletes.
    assert!(
        deletion::pick_deletion_fix(true, &file, &ir_span(0, 16), PubVerdict::IntraCrate).is_none()
    );
    // Unused + opt-in on an untracked file → the Skip suggestion (with note).
    let (sugg, note) =
        deletion::pick_deletion_fix(true, &file, &ir_span(0, 16), PubVerdict::Unused)
            .expect("Some");
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
