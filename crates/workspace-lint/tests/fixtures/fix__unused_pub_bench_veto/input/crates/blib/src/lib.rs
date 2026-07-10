/// Referenced only from blib's own bench: unused in every declared config,
/// but the bench-mention veto must keep it.
pub fn bench_only() -> i32 {
    120
}

/// Mentioned nowhere at all — deleted.
pub fn dead_helper() -> i32 {
    0
}

pub fn always_used() -> i32 {
    1
}
