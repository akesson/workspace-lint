use super::helpers::*;
use crate::unused_pub::*;
use scip::types::SymbolRole;
use scip::types::symbol_information::Kind;

#[test]
fn decl_map_collects_all_kinds() {
    let sym_fn = "rust-analyzer cargo mycrate 0.1.0 mycrate/foo()";
    let sym_struct = "rust-analyzer cargo mycrate 0.1.0 mycrate/Foo#";
    let sym_enum = "rust-analyzer cargo mycrate 0.1.0 mycrate/Bar#";
    let sym_const = "rust-analyzer cargo mycrate 0.1.0 mycrate/X.";
    let sym_trait = "rust-analyzer cargo mycrate 0.1.0 mycrate/Baz#";
    let sym_type = "rust-analyzer cargo mycrate 0.1.0 mycrate/Alias#";

    let doc = make_doc(
        "src/lib.rs",
        vec![
            make_symbol(sym_fn, Kind::Function, "foo"),
            make_symbol(sym_struct, Kind::Struct, "Foo"),
            make_symbol(sym_enum, Kind::Enum, "Bar"),
            make_symbol(sym_const, Kind::Constant, "X"),
            make_symbol(sym_trait, Kind::Trait, "Baz"),
            make_symbol(sym_type, Kind::TypeAlias, "Alias"),
        ],
        vec![
            make_occurrence(sym_fn, SymbolRole::Definition as i32, 0),
            make_occurrence(sym_struct, SymbolRole::Definition as i32, 1),
            make_occurrence(sym_enum, SymbolRole::Definition as i32, 2),
            make_occurrence(sym_const, SymbolRole::Definition as i32, 3),
            make_occurrence(sym_trait, SymbolRole::Definition as i32, 4),
            make_occurrence(sym_type, SymbolRole::Definition as i32, 5),
        ],
    );
    let index = make_index(vec![doc]);
    let map = build_declaration_map(&index);

    assert_eq!(map.len(), 6);
    assert_eq!(map[sym_fn].kind, Kind::Function);
    assert_eq!(map[sym_struct].kind, Kind::Struct);
    assert_eq!(map[sym_enum].kind, Kind::Enum);
    assert_eq!(map[sym_const].kind, Kind::Constant);
    assert_eq!(map[sym_trait].kind, Kind::Trait);
    assert_eq!(map[sym_type].kind, Kind::TypeAlias);
}

#[test]
fn decl_map_skips_local_symbols() {
    let doc = make_doc(
        "src/lib.rs",
        vec![make_symbol("local 42", Kind::Variable, "x")],
        vec![],
    );
    let index = make_index(vec![doc]);
    let map = build_declaration_map(&index);
    assert!(map.is_empty());
}

#[test]
fn decl_map_skips_empty_symbols() {
    let doc = make_doc(
        "src/lib.rs",
        vec![make_symbol("", Kind::Variable, "x")],
        vec![],
    );
    let index = make_index(vec![doc]);
    let map = build_declaration_map(&index);
    assert!(map.is_empty());
}

#[test]
fn decl_map_records_definition_line() {
    let sym = "rust-analyzer cargo mycrate 0.1.0 mycrate/foo()";
    let doc = make_doc(
        "src/lib.rs",
        vec![make_symbol(sym, Kind::Function, "foo")],
        vec![make_occurrence(sym, SymbolRole::Definition as i32, 10)],
    );
    let index = make_index(vec![doc]);
    let map = build_declaration_map(&index);
    assert_eq!(map[sym].definition_line, Some(10));
}

#[test]
fn decl_map_records_def_symbol_roles() {
    let sym = "rust-analyzer cargo mycrate 0.1.0 mycrate/test_foo()";
    let roles = SymbolRole::Definition as i32 | SymbolRole::Test as i32; // 33
    let doc = make_doc(
        "src/lib.rs",
        vec![make_symbol(sym, Kind::Function, "test_foo")],
        vec![make_occurrence(sym, roles, 5)],
    );
    let index = make_index(vec![doc]);
    let map = build_declaration_map(&index);
    assert_eq!(map[sym].def_symbol_roles, 33);
}

#[test]
fn decl_map_multiple_documents() {
    let sym_a = "rust-analyzer cargo mycrate 0.1.0 mycrate/a()";
    let sym_b = "rust-analyzer cargo mycrate 0.1.0 mycrate/b()";
    let doc_a = make_doc(
        "src/a.rs",
        vec![make_symbol(sym_a, Kind::Function, "a")],
        vec![make_occurrence(sym_a, SymbolRole::Definition as i32, 0)],
    );
    let doc_b = make_doc(
        "src/b.rs",
        vec![make_symbol(sym_b, Kind::Function, "b")],
        vec![make_occurrence(sym_b, SymbolRole::Definition as i32, 0)],
    );
    let index = make_index(vec![doc_a, doc_b]);
    let map = build_declaration_map(&index);
    assert_eq!(map.len(), 2);
    assert_eq!(map[sym_a].relative_path, "src/a.rs");
    assert_eq!(map[sym_b].relative_path, "src/b.rs");
}

#[test]
fn decl_map_duplicate_prefers_definition_line() {
    let sym = "rust-analyzer cargo mycrate 0.1.0 mycrate/foo()";
    // First doc has no definition occurrence (definition_line = None)
    let doc_a = make_doc(
        "src/a.rs",
        vec![make_symbol(sym, Kind::Function, "foo")],
        vec![],
    );
    // Second doc has a definition occurrence
    let doc_b = make_doc(
        "src/b.rs",
        vec![make_symbol(sym, Kind::Function, "foo")],
        vec![make_occurrence(sym, SymbolRole::Definition as i32, 5)],
    );
    let index = make_index(vec![doc_a, doc_b]);
    let map = build_declaration_map(&index);
    assert_eq!(map.len(), 1);
    assert_eq!(map[sym].definition_line, Some(5));
    assert_eq!(map[sym].relative_path, "src/b.rs");
}
