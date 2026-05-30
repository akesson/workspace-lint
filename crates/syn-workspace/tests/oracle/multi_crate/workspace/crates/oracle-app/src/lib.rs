//! oracle-app — references `oracle-core` so the SCIP set-level dependency oracle
//! has complete ground truth that `oracle-core` is used (guards `unused-deps`
//! against a false positive).

use oracle_core::{café, deep, geometry, Shape, Widget};

pub fn run() -> u32 {
    let w = Widget { size: 7 };
    let _shape = Shape::Circle;
    w.render() + deep() + café() + geometry::area() + oracle_core::top_level()
}
