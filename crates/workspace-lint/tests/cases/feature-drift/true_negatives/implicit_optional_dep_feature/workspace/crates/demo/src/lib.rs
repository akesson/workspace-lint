// No `[features]` table and no `#[cfg(feature = ...)]` gates — the only
// feature in cargo metadata is the implicit `optdep` one, which must not be
// flagged.
pub fn hello() {}
