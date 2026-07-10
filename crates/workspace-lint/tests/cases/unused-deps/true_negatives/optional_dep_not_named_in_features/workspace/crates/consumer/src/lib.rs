//! Declares `provider` as `optional = true` but has no `[features]` table —
//! no feature-plumbing evidence exists, and no code names it. The dep is
//! feature-gated (cargo's implicit `provider` feature), hence unobservable
//! under configs that don't enable it: never flagged.

pub fn run() {}
