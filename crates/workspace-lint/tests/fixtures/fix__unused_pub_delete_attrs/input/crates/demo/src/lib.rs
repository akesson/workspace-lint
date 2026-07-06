use util::thing;

/// Reached by the `app` bin (cross-crate) → stays alive and `pub`.
pub fn entry() -> u32 {
    1
}

#[cfg(test)]
pub fn fake() -> u32 {
    2
}

#[deprecated(note = "old")]
#[must_use]
pub fn stale() -> u32 {
    thing()
}
