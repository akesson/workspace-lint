use super::*;

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
