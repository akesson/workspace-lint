//! oracle-core — the rich fixture crate the differential oracle validates.
//!
//! Each item probes a specific resolver behaviour (see tests/oracle.rs):
//!   - `top_level`, `Shape`, `geometry::*` : module-level public defs that
//!     both syn-workspace and rustdoc must agree on (def/visibility oracle).
//!   - `Widget::render` : a `pub` method inside an `impl` block — known
//!     rustdoc-only (syn enumerates module-level items only).
//!   - `deep` (re-export) : single-hop `pub use` whose canonical target is
//!     `oracle_core::internal::deep` (re-export cross-validation).
//!   - `café` : non-ASCII identifier — encoding regression guard.

/// Plain public fn — present in both oracles.
pub fn top_level() -> u32 {
    1
}

/// Public struct; its method lives in an `impl` block (known rustdoc-only).
pub struct Widget {
    pub size: u32,
}

impl Widget {
    /// `pub` method inside `impl` — syn-workspace does not enumerate it, so the
    /// oracle records it as a known divergence rather than a regression.
    pub fn render(&self) -> u32 {
        self.size
    }
}

/// Public enum — a module-level def in both oracles.
pub enum Shape {
    Circle,
    Square,
}

/// Nested public module with its own public items (deeper canonical paths).
pub mod geometry {
    pub fn area() -> u32 {
        0
    }

    pub struct Point {
        pub x: u32,
        pub y: u32,
    }
}

mod internal {
    pub fn deep() -> u32 {
        42
    }
}

/// Single-hop public re-export — canonical target is `oracle_core::internal::deep`.
pub use internal::deep;

/// Non-ASCII identifier — the byte length (5) differs from the char length (4),
/// guarding against SCIP/rustdoc range-encoding drift.
pub fn café() -> u32 {
    0
}
