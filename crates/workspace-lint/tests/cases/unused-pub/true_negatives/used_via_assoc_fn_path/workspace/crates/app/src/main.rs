// The only cross-crate reference to `Gamma` is through an associated-fn
// path — no `use corelib::Gamma` binding anywhere. Prefix crediting must
// count `corelib::Gamma::make` as a use of `corelib::Gamma`.
fn main() {
    let _ = corelib::Gamma::make();
}
