/// Used by `app` — keeps the glob non-empty.
pub struct Allowance {
    pub days: u32,
}

/// Dead — deleted: the glob still re-exports `Allowance`.
pub struct ProrateData {
    pub fraction: u32,
}
