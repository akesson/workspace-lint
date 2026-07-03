pub mod extra {
    /// Only `helper` calls this — when `helper` cascades dead, so does this.
    /// Nested + indented: proves the delete eats the leading indent cleanly.
    pub fn inner_dead() -> u32 {
        9
    }

    /// Reached transitively by `kept` → stays (tightened to `pub(crate)`).
    pub fn extra_kept() -> u32 {
        10
    }
}

pub fn helper() -> u32 {
    extra::inner_dead()
}

pub fn kept() -> u32 {
    extra::extra_kept()
}
