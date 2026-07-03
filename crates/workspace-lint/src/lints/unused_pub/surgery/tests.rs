//! Import-surgery byte math: brace-leaf excision, whole-statement deletion,
//! and the coalescing that keeps adjacent dead leaves from producing
//! overlapping (file-aborting) suggestions. Each case applies the computed
//! deletions to real source and asserts the result compiles-shaped.

use super::*;
use tempfile::TempDir;
use wl_engine::wl_ir::Span as IrSpan;

/// A `DanglingImport` whose spans point at real byte ranges in `src`. `leaf` is
/// the leaf text (must occur once); `stmt` is the whole `use …;` (its own text
/// for a standalone import, or the same as `leaf` for a brace-list leaf, which
/// is how the extractor collapses it).
fn dangling(src: &str, leaf: &str, stmt: &str) -> DanglingImport {
    let elo = src.find(leaf).expect("leaf in src") as u32;
    let elem = IrSpan {
        file: "src/lib.rs".into(),
        lo: elo,
        hi: elo + leaf.len() as u32,
        line: 1,
        from_expansion: false,
    };
    let slo = src.find(stmt).expect("stmt in src") as u32;
    let decl = IrSpan {
        lo: slo,
        hi: slo + stmt.len() as u32,
        ..elem.clone()
    };
    DanglingImport {
        decl,
        elem,
        reexport: false,
    }
}

/// Run `import_surgery` over `src` and apply every emitted deletion (descending
/// byte order, so earlier offsets stay valid) — the same replace `fix::run`
/// performs.
fn apply(src: &str, dangling: Vec<DanglingImport>) -> String {
    let tmp = TempDir::new().unwrap();
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    std::fs::write(tmp.path().join("src/lib.rs"), src).unwrap();
    let diags = import_surgery(dangling, tmp.path());
    let mut spans: Vec<(u32, u32)> = diags
        .iter()
        .flat_map(|d| &d.suggestions)
        .map(|s| (s.span.byte_start, s.span.byte_end))
        .collect();
    spans.sort_by_key(|(lo, _)| std::cmp::Reverse(*lo));
    let mut out = src.to_string();
    for (lo, hi) in spans {
        out.replace_range(lo as usize..hi as usize, "");
    }
    out
}

#[test]
fn standalone_import_deletes_whole_statement() {
    let src = "use a::b;\nuse a::c;\n";
    // `b` is removed; the whole `use a::b;` (decl ⊋ elem) plus its newline go.
    let out = apply(src, vec![dangling(src, "b;", "use a::b;")]);
    assert_eq!(out, "use a::c;\n");
}

#[test]
fn brace_leaf_first_removes_leaf_and_trailing_separator() {
    let src = "use a::{b, c};\n";
    // Brace leaf: decl == elem == "b".
    let out = apply(src, vec![dangling(src, "b", "b")]);
    assert_eq!(out, "use a::{c};\n");
}

#[test]
fn brace_leaf_last_removes_leaf_and_leading_separator() {
    let src = "use a::{b, c};\n";
    let out = apply(src, vec![dangling(src, "c", "c")]);
    assert_eq!(out, "use a::{b};\n");
}

#[test]
fn brace_all_leaves_coalesce_to_empty_group() {
    let src = "use a::{b, c};\n";
    // Both dead: the two separator ranges overlap and must coalesce, not abort.
    let out = apply(src, vec![dangling(src, "b", "b"), dangling(src, "c", "c")]);
    assert_eq!(out, "use a::{};\n");
}

#[test]
fn brace_middle_leaf_of_three() {
    let src = "use a::{b, c, d};\n";
    let out = apply(src, vec![dangling(src, "c", "c")]);
    assert_eq!(out, "use a::{b, d};\n");
}

#[test]
fn sole_brace_leaf_leaves_empty_group() {
    let src = "use a::{b};\n";
    let out = apply(src, vec![dangling(src, "b", "b")]);
    assert_eq!(out, "use a::{};\n");
}

#[test]
fn crlf_standalone_deletes_both_newline_bytes() {
    let src = "use a::b;\r\nuse a::c;\r\n";
    let out = apply(src, vec![dangling(src, "b;", "use a::b;")]);
    assert_eq!(out, "use a::c;\r\n");
}

#[test]
fn macro_generated_and_reexport_imports_are_skipped() {
    let src = "use a::{b, c};\n";
    let mut mac = dangling(src, "b", "b");
    mac.decl.from_expansion = true;
    let mut rex = dangling(src, "c", "c");
    rex.reexport = true;
    // Neither is excisable → no deletions emitted, source untouched.
    let out = apply(src, vec![mac, rex]);
    assert_eq!(out, src);
}
