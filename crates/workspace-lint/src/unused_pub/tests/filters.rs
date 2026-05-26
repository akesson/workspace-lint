use crate::unused_pub::*;
use scip::types::SymbolRole;
use scip::types::symbol_information::Kind;

// ── E. Filter functions ──

#[test]
fn is_test_symbol_true() {
    let entry = SymbolEntry {
        symbol: String::new(),
        kind: Kind::Function,
        display_name: String::new(),
        relative_path: String::new(),

        is_used: false,
        used_same_crate: false,
        used_cross_crate: false,
        definition_line: Some(0),
        def_symbol_roles: SymbolRole::Test as i32,
        is_impl: false,
    };
    assert!(is_test_symbol(&entry));
}

#[test]
fn is_test_symbol_false() {
    let entry = SymbolEntry {
        symbol: String::new(),
        kind: Kind::Function,
        display_name: String::new(),
        relative_path: String::new(),

        is_used: false,
        used_same_crate: false,
        used_cross_crate: false,
        definition_line: Some(0),
        def_symbol_roles: SymbolRole::Definition as i32,
        is_impl: false,
    };
    assert!(!is_test_symbol(&entry));
}

#[test]
fn is_test_symbol_combined_bits() {
    let entry = SymbolEntry {
        symbol: String::new(),
        kind: Kind::Function,
        display_name: String::new(),
        relative_path: String::new(),

        is_used: false,
        used_same_crate: false,
        used_cross_crate: false,
        definition_line: Some(0),
        def_symbol_roles: SymbolRole::Definition as i32 | SymbolRole::Test as i32,
        is_impl: false,
    };
    assert!(is_test_symbol(&entry));
}

#[test]
fn excludes_main_function() {
    let entry = SymbolEntry {
        symbol: String::new(),
        kind: Kind::Function,
        display_name: "main".to_string(),
        relative_path: String::new(),

        is_used: false,
        used_same_crate: false,
        used_cross_crate: false,
        definition_line: Some(0),
        def_symbol_roles: 0,
        is_impl: false,
    };
    assert!(is_main_function(&entry));
}

#[test]
fn does_not_exclude_non_main() {
    let entry = SymbolEntry {
        symbol: String::new(),
        kind: Kind::Function,
        display_name: "run".to_string(),
        relative_path: String::new(),

        is_used: false,
        used_same_crate: false,
        used_cross_crate: false,
        definition_line: Some(0),
        def_symbol_roles: 0,
        is_impl: false,
    };
    assert!(!is_main_function(&entry));
}

#[test]
fn does_not_exclude_main_struct() {
    let entry = SymbolEntry {
        symbol: String::new(),
        kind: Kind::Struct,
        display_name: "main".to_string(),
        relative_path: String::new(),

        is_used: false,
        used_same_crate: false,
        used_cross_crate: false,
        definition_line: Some(0),
        def_symbol_roles: 0,
        is_impl: false,
    };
    assert!(!is_main_function(&entry));
}

#[test]
fn is_trait_impl_true() {
    let entry = SymbolEntry {
        symbol: String::new(),
        kind: Kind::Method,
        display_name: String::new(),
        relative_path: String::new(),
        is_used: false,
        used_same_crate: false,
        used_cross_crate: false,
        definition_line: Some(0),
        def_symbol_roles: 0,
        is_impl: true,
    };
    assert!(is_trait_impl(&entry));
}

#[test]
fn is_trait_impl_false() {
    let entry = SymbolEntry {
        symbol: String::new(),
        kind: Kind::Method,
        display_name: String::new(),
        relative_path: String::new(),

        is_used: false,
        used_same_crate: false,
        used_cross_crate: false,
        definition_line: Some(0),
        def_symbol_roles: 0,
        is_impl: false,
    };
    assert!(!is_trait_impl(&entry));
}

#[test]
fn is_trait_impl_reference_only() {
    let entry = SymbolEntry {
        symbol: String::new(),
        kind: Kind::Method,
        display_name: String::new(),
        relative_path: String::new(),
        is_used: false,
        used_same_crate: false,
        used_cross_crate: false,
        definition_line: Some(0),
        def_symbol_roles: 0,
        is_impl: false,
    };
    assert!(!is_trait_impl(&entry));
}

// ── F. Crate name extraction ──

#[test]
fn extract_crate_standard() {
    assert_eq!(
        extract_crate_name("rust-analyzer cargo my-crate 0.1.0 my_crate/Foo#"),
        Some("my-crate")
    );
}

#[test]
fn extract_crate_hyphenated() {
    assert_eq!(
        extract_crate_name("rust-analyzer cargo foo-bar-baz 1.0.0 foo_bar_baz/something()"),
        Some("foo-bar-baz")
    );
}

#[test]
fn extract_crate_malformed() {
    assert_eq!(extract_crate_name("local 42"), None);
}

#[test]
fn extract_crate_empty() {
    assert_eq!(extract_crate_name(""), None);
}

// ── G. Kind filter parsing ──

#[test]
fn parse_kinds_empty() {
    assert!(parse_kind_filter(&[]).is_none());
}

#[test]
fn parse_kinds_function() {
    let filter = parse_kind_filter(&["function".to_string()]).unwrap();
    assert!(filter.contains(&Kind::Function));
    assert!(!filter.contains(&Kind::Struct));
}

#[test]
fn parse_kinds_method_expands() {
    let filter = parse_kind_filter(&["method".to_string()]).unwrap();
    assert!(filter.contains(&Kind::Method));
    assert!(filter.contains(&Kind::StaticMethod));
}

#[test]
fn parse_kinds_multiple() {
    let filter = parse_kind_filter(&[
        "function".to_string(),
        "struct".to_string(),
        "enum".to_string(),
    ])
    .unwrap();
    assert!(filter.contains(&Kind::Function));
    assert!(filter.contains(&Kind::Struct));
    assert!(filter.contains(&Kind::Enum));
}

// ── H. Allowlist matching ──

#[test]
fn allowlist_exact_match() {
    let gs = build_allowlist(&["Error".to_string()]).unwrap();
    assert!(gs.is_match("Error"));
}

#[test]
fn allowlist_glob_pattern() {
    let gs = build_allowlist(&["*Error".to_string()]).unwrap();
    assert!(gs.is_match("MyError"));
}

#[test]
fn allowlist_no_match() {
    let gs = build_allowlist(&["Error".to_string()]).unwrap();
    assert!(!gs.is_match("Foo"));
}

#[test]
fn allowlist_empty() {
    assert!(build_allowlist(&[]).is_none());
}
