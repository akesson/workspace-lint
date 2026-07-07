//! P3: `outer!` expands solely to `inner!` — the expansion-chain walk must
//! credit BOTH macros (the innermost `ExpnData` alone names only `inner`).
macro_rules! inner {
    () => {
        7
    };
}
macro_rules! outer {
    () => {
        inner!()
    };
}

pub fn chained() -> i32 {
    outer!()
}
