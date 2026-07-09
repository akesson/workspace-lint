pub fn helper() -> u32 {
    1
}

// Private (and `allow(dead_code)`): supplies the intra-crate PRODUCTION edge
// without adding an unused-pub candidate of its own.
#[allow(dead_code)]
fn shipping_caller() -> u32 {
    helper() + 1
}
