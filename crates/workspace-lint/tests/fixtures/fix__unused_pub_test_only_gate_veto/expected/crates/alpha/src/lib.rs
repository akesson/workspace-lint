pub fn kept() -> u32 {
    2
}

/// Only `beta`'s test calls this — exclusive scaffolding at the engine
/// level, but the test fn is allowlisted (out of fix scope), so nothing
/// may be deleted.
pub fn embalmed() -> u32 {
    1
}
