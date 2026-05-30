//! oracle-app — references `oracle-core` so the SCIP set-level dependency oracle
//! has complete ground truth that `oracle-core` is used (guards `unused-deps`
//! against a false positive).

use oracle_core::{café, deep, geometry, Shape, Widget};

pub fn run() -> u32 {
    let w = Widget { size: 7 };
    let _shape = Shape::Circle;
    w.render() + deep() + café() + geometry::area() + oracle_core::top_level()
}

#[cfg(test)]
mod tests {
    /// References `oracle-extra` so it is a genuinely-used DEV dependency — the
    /// dep-set oracle must still classify it as dev-only and exclude it from the
    /// normal `[dependencies]` set it checks.
    #[test]
    fn uses_extra() {
        assert_eq!(oracle_extra::gadget(), 0);
    }
}
