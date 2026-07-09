pub fn kept() -> u32 {
    2
}

/// Only `beta`'s `#[cfg(test)]` module calls this.
pub fn embalmed() -> u32 {
    1
}

/// Only `beta`'s integration test calls this.
pub fn it_only() -> u32 {
    3
}
