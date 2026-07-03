//! Demo crate with no dependencies, so `centralized-deps` never fires — the
//! injected `expect(centralized-deps)` directive is therefore stale.

pub fn demo() {}
