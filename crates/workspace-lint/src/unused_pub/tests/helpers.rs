use scip::types::symbol_information::Kind;
use scip::types::{Document, Index, Occurrence, Relationship, SymbolInformation};

pub(super) fn make_index(docs: Vec<Document>) -> Index {
    let mut index = Index::new();
    index.documents = docs;
    index
}

pub(super) fn make_symbol(symbol: &str, kind: Kind, display_name: &str) -> SymbolInformation {
    let mut si = SymbolInformation::new();
    si.symbol = symbol.to_string();
    si.kind = kind.into();
    si.display_name = display_name.to_string();
    si
}

pub(super) fn make_symbol_with_rels(
    symbol: &str,
    kind: Kind,
    display_name: &str,
    relationships: Vec<Relationship>,
) -> SymbolInformation {
    let mut si = make_symbol(symbol, kind, display_name);
    si.relationships = relationships;
    si
}

pub(super) fn make_occurrence(symbol: &str, roles: i32, line: i32) -> Occurrence {
    let mut occ = Occurrence::new();
    occ.symbol = symbol.to_string();
    occ.symbol_roles = roles;
    occ.range = vec![line, 0, 10]; // [startLine, startChar, endChar]
    occ
}

pub(super) fn make_doc(
    path: &str,
    symbols: Vec<SymbolInformation>,
    occurrences: Vec<Occurrence>,
) -> Document {
    let mut doc = Document::new();
    doc.relative_path = path.to_string();
    doc.symbols = symbols;
    doc.occurrences = occurrences;
    doc
}

pub(super) fn make_relationship(is_implementation: bool) -> Relationship {
    let mut rel = Relationship::new();
    rel.is_implementation = is_implementation;
    rel
}
