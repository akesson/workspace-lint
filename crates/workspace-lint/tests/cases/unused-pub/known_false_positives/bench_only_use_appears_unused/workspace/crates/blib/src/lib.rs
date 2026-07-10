/// Referenced only from blib's own bench, which no declared config compiles.
pub fn bench_only() -> i32 {
    120
}

/// Referenced from same-crate production code AND from the bench (the
/// ripgrep `is_match_candidate` shape): the verdict reads intra-crate — the
/// bench use is invisible — and the `pub(crate)` narrow must be withheld,
/// or `--fix` breaks `cargo bench`.
pub fn prod_and_bench() -> i32 {
    7
}

pub fn always_used() -> i32 {
    1 + prod_and_bench()
}
