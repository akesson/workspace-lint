// The ONLY use of `beta` in this crate. The macro re-emits these item
// tokens verbatim, so nothing in the compiled HIR names `beta`.
beta::passthrough! {
    pub fn from_macro() -> u32 {
        11
    }
}
