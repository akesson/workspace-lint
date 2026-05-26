use super::helpers::*;
use crate::unused_pub::*;
use scip::types::SymbolRole;
use scip::types::symbol_information::Kind;

#[test]
fn mark_used_reference_occurrence() {
    let sym = "rust-analyzer cargo mycrate 0.1.0 mycrate/foo()";
    let doc = make_doc(
        "src/lib.rs",
        vec![make_symbol(sym, Kind::Function, "foo")],
        vec![
            make_occurrence(sym, SymbolRole::Definition as i32, 0),
            make_occurrence(sym, 0, 10), // plain reference
        ],
    );
    let index = make_index(vec![doc]);
    let mut map = build_declaration_map(&index);
    mark_used_symbols(&index, &mut map);
    assert!(map[sym].is_used);
}

#[test]
fn mark_used_import_occurrence() {
    let sym = "rust-analyzer cargo mycrate 0.1.0 mycrate/Foo#";
    let doc = make_doc(
        "src/lib.rs",
        vec![make_symbol(sym, Kind::Struct, "Foo")],
        vec![
            make_occurrence(sym, SymbolRole::Definition as i32, 0),
            make_occurrence(sym, SymbolRole::Import as i32, 5),
        ],
    );
    let index = make_index(vec![doc]);
    let mut map = build_declaration_map(&index);
    mark_used_symbols(&index, &mut map);
    assert!(map[sym].is_used);
}

#[test]
fn mark_used_read_access() {
    let sym = "rust-analyzer cargo mycrate 0.1.0 mycrate/X.";
    let doc = make_doc(
        "src/lib.rs",
        vec![make_symbol(sym, Kind::Constant, "X")],
        vec![
            make_occurrence(sym, SymbolRole::Definition as i32, 0),
            make_occurrence(sym, SymbolRole::ReadAccess as i32, 8),
        ],
    );
    let index = make_index(vec![doc]);
    let mut map = build_declaration_map(&index);
    mark_used_symbols(&index, &mut map);
    assert!(map[sym].is_used);
}

#[test]
fn not_marked_by_definition_only() {
    let sym = "rust-analyzer cargo mycrate 0.1.0 mycrate/foo()";
    let doc = make_doc(
        "src/lib.rs",
        vec![make_symbol(sym, Kind::Function, "foo")],
        vec![make_occurrence(sym, SymbolRole::Definition as i32, 0)],
    );
    let index = make_index(vec![doc]);
    let mut map = build_declaration_map(&index);
    mark_used_symbols(&index, &mut map);
    assert!(!map[sym].is_used);
}

#[test]
fn cross_document_reference() {
    let sym = "rust-analyzer cargo mycrate 0.1.0 mycrate/foo()";
    let doc_a = make_doc(
        "src/a.rs",
        vec![make_symbol(sym, Kind::Function, "foo")],
        vec![make_occurrence(sym, SymbolRole::Definition as i32, 0)],
    );
    let doc_b = make_doc(
        "src/b.rs",
        vec![],
        vec![make_occurrence(sym, 0, 3)], // plain reference in another file
    );
    let index = make_index(vec![doc_a, doc_b]);
    let mut map = build_declaration_map(&index);
    mark_used_symbols(&index, &mut map);
    assert!(map[sym].is_used);
}

#[test]
fn unknown_symbol_in_occurrence() {
    let sym = "rust-analyzer cargo mycrate 0.1.0 mycrate/foo()";
    let doc = make_doc(
        "src/lib.rs",
        vec![make_symbol(sym, Kind::Function, "foo")],
        vec![
            make_occurrence(sym, SymbolRole::Definition as i32, 0),
            make_occurrence("rust-analyzer cargo mycrate 0.1.0 mycrate/unknown()", 0, 5),
        ],
    );
    let index = make_index(vec![doc]);
    let mut map = build_declaration_map(&index);
    mark_used_symbols(&index, &mut map);
    // No crash, and foo is still unused
    assert!(!map[sym].is_used);
}
