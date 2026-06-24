// Fixture for the signature-exposure walk (`module_tree/signature.rs`). Every
// "leaked" type lives in a private `mod inner` so its canonical is distinct and
// unambiguous; the asserting test (`signature_exposure_*` in
// `module_tree/tests.rs`) checks exactly which ones the walk records.

pub trait Marker {
    type Out;
}

pub struct Endpoint;

impl Marker for Endpoint {
    // Trait-impl associated type — the E0446 trigger.
    type Out = inner::AssocType;
}

// Public fn return type.
pub fn ret() -> inner::RetType {
    inner::RetType
}

// Public fn parameter type.
pub fn param(_v: inner::ParamType) {}

// Type nested inside a generic argument (`Vec<…>`).
pub fn nested() -> Vec<inner::NestedType> {
    Vec::new()
}

// Public field of a public struct.
pub struct Holder {
    pub field: inner::FieldType,
}

// --- negatives: must NOT be recorded as Public signature exposures ---

// Referenced only from a fn body, never a signature position.
pub fn uses_body() {
    let _x = inner::BodyOnly;
}

// Exposer is `pub(crate)`, not `Public`.
pub(crate) fn crate_ret() -> inner::CrateOnlyType {
    inner::CrateOnlyType
}

// `pub` field but the containing struct is private, so nothing leaks publicly.
struct PrivateHolder {
    pub g: inner::PrivFieldType,
}

mod inner {
    pub struct AssocType;
    pub struct RetType;
    pub struct ParamType;
    pub struct NestedType;
    pub struct FieldType;
    pub struct BodyOnly;
    pub struct CrateOnlyType;
    pub struct PrivFieldType;
}
