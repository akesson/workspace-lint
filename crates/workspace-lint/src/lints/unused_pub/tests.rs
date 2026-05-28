use super::*;

#[test]
fn kind_filter_parses_aliases() {
    let filter = parse_kind_filter(&["fn".into(), "function".into(), "type".into()]).unwrap();
    assert!(filter.contains(&ItemKind::Fn));
    assert!(filter.contains(&ItemKind::TypeAlias));
}

#[test]
fn kind_filter_ignores_unknown_kinds() {
    let filter = parse_kind_filter(&["banana".into(), "fn".into()]).unwrap();
    assert_eq!(filter.len(), 1);
    assert!(filter.contains(&ItemKind::Fn));
}

#[test]
fn kind_filter_empty_returns_none() {
    assert!(parse_kind_filter(&[]).is_none());
}

#[test]
fn glob_set_returns_none_for_empty() {
    assert!(build_glob_set(&[], "test").is_none());
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
