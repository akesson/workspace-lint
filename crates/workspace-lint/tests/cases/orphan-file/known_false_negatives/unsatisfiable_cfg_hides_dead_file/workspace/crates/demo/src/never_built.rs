// Genuinely dead: reachable only through an `#[cfg(all(unix, windows))]` gate,
// which no target satisfies. Nothing here is ever compiled or checked.
pub fn unreachable_helper() -> u32 {
    0
}
