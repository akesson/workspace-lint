// `legacy` appears only inside a `not(...)` predicate — feature-drift must
// still count it as gated.
#[cfg(not(feature = "legacy"))]
pub fn modern() {}
