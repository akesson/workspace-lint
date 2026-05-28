// build.rs references `provider`, but the resolver doesn't scan build.rs.
// The unused-deps check therefore false-positive-flags `provider` even
// though it's genuinely used. Documented as a known limitation.
pub fn nothing() {}
