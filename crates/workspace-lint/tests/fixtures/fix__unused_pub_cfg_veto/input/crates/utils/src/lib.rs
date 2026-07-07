/// Referenced only from app's wasm-cfg-gated block: unused in every declared
/// config, but the shadow veto must keep it.
pub fn tz_offset_minutes() -> i32 {
    120
}

/// Mentioned nowhere at all — deleted.
pub fn dead_helper() -> i32 {
    0
}

pub fn always_used() -> i32 {
    1
}
