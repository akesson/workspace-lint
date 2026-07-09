pub fn kept() -> u32 {
    2
}

/// Only `beta`'s test calls this — but that test also exercises `kept`,
/// so it is not exclusive scaffolding and nothing may be deleted.
pub fn embalmed() -> u32 {
    1
}
