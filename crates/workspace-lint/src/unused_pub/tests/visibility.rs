use crate::unused_pub::collect_pub_items_from_source;

#[test]
fn detects_pub_fn() {
    let result = collect_pub_items_from_source("pub fn foo() {}\n");
    assert_eq!(result.lines, vec![0]);
}

#[test]
fn ignores_private_fn() {
    let result = collect_pub_items_from_source("fn foo() {}\n");
    assert!(result.lines.is_empty());
}

#[test]
fn ignores_pub_crate_fn() {
    let result = collect_pub_items_from_source("pub(crate) fn foo() {}\n");
    assert!(result.lines.is_empty());
}

#[test]
fn ignores_pub_super_fn() {
    let result = collect_pub_items_from_source("pub(super) fn foo() {}\n");
    assert!(result.lines.is_empty());
}

#[test]
fn detects_pub_struct() {
    let result = collect_pub_items_from_source("pub struct Foo;\n");
    assert_eq!(result.lines, vec![0]);
}

#[test]
fn detects_pub_enum() {
    let result = collect_pub_items_from_source("pub enum Bar {}\n");
    assert_eq!(result.lines, vec![0]);
}

#[test]
fn detects_pub_const() {
    let result = collect_pub_items_from_source("pub const X: i32 = 1;\n");
    assert_eq!(result.lines, vec![0]);
}

#[test]
fn detects_pub_static() {
    let result = collect_pub_items_from_source("pub static Y: i32 = 1;\n");
    assert_eq!(result.lines, vec![0]);
}

#[test]
fn detects_pub_trait() {
    let result = collect_pub_items_from_source("pub trait Baz {}\n");
    assert_eq!(result.lines, vec![0]);
}

#[test]
fn detects_pub_type_alias() {
    let result = collect_pub_items_from_source("pub type Alias = i32;\n");
    assert_eq!(result.lines, vec![0]);
}

#[test]
fn detects_pub_mod() {
    let result = collect_pub_items_from_source("pub mod inner {}\n");
    assert_eq!(result.lines, vec![0]);
}

#[test]
fn detects_pub_impl_method() {
    let src = "struct Foo;\nimpl Foo {\n    pub fn bar() {}\n}\n";
    let result = collect_pub_items_from_source(src);
    assert_eq!(result.lines, vec![2]); // 0-based line 2
}

#[test]
fn ignores_private_impl_method() {
    let src = "struct Foo;\nimpl Foo {\n    fn bar() {}\n}\n";
    let result = collect_pub_items_from_source(src);
    assert!(result.lines.is_empty());
}

#[test]
fn detects_pub_impl_const() {
    let src = "struct Foo;\nimpl Foo {\n    pub const X: i32 = 1;\n}\n";
    let result = collect_pub_items_from_source(src);
    assert_eq!(result.lines, vec![2]);
}

#[test]
fn nested_mod_pub_items() {
    let src = "pub mod m {\n    pub fn f() {}\n    fn g() {}\n}\n";
    let result = collect_pub_items_from_source(src);
    // pub mod at line 0, pub fn f at line 1
    assert_eq!(result.lines, vec![0, 1]);
}

#[test]
fn line_number_accuracy() {
    let src = "\
fn private1() {}
pub fn public1() {}
fn private2() {}
pub struct Foo;
fn private3() {}
pub const X: i32 = 1;
";
    let result = collect_pub_items_from_source(src);
    assert_eq!(result.lines, vec![1, 3, 5]); // 0-based
}

#[test]
fn detects_pub_use() {
    let result = collect_pub_items_from_source("pub use std::collections::HashMap;\n");
    assert_eq!(result.lines, vec![0]);
}

#[test]
fn ignores_private_use() {
    let result = collect_pub_items_from_source("use std::collections::HashMap;\n");
    assert!(result.lines.is_empty());
}

#[test]
fn derive_serialize_suppresses_struct() {
    let src = "#[derive(Serialize)]\npub struct Foo { pub x: i32 }\n";
    let result = collect_pub_items_from_source(src);
    // Line 1: pub struct Foo, also line 1: pub x field (same line in this case)
    assert!(result.lines.contains(&1));
    assert!(result.derive_suppressed_lines.contains(&1));
}

#[test]
fn detects_pub_struct_field() {
    let src = "pub struct Foo {\n    pub x: i32,\n    y: bool,\n}\n";
    let result = collect_pub_items_from_source(src);
    // line 0: pub struct Foo, line 1: pub x field
    assert_eq!(result.lines, vec![0, 1]);
}

#[test]
fn ignores_private_struct_field() {
    let src = "pub struct Foo {\n    x: i32,\n}\n";
    let result = collect_pub_items_from_source(src);
    // Only the struct itself, not the private field
    assert_eq!(result.lines, vec![0]);
}

#[test]
fn derive_non_suppressing_not_marked() {
    let src = "#[derive(Debug)]\npub struct Foo;\n";
    let result = collect_pub_items_from_source(src);
    assert_eq!(result.lines, vec![1]);
    assert!(result.derive_suppressed_lines.is_empty());
}
