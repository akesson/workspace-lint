// `orphan` never references `provider`, but the scoped [engine] matrix never
// compiles `orphan`, so its dep is a coverage gap — not a false "unused".
pub fn standalone() -> u32 {
    7
}
