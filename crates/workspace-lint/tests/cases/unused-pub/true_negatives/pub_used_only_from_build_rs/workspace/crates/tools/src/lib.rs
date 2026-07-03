/// Consumed ONLY from crates/consumer's build.rs — a build-script unit whose
/// references must count as uses (the engine joins them by display path: the
/// Build-mode compile carries a different DefPathHash generation).
pub fn copy_if_changed() -> u32 {
    7
}
