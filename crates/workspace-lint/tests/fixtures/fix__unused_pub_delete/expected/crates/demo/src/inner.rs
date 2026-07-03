pub mod extra {

    /// Reached transitively by `kept` → stays (tightened to `pub(crate)`).
    pub(crate) fn extra_kept() -> u32 {
        10
    }
}


pub(crate) fn kept() -> u32 {
    extra::extra_kept()
}
