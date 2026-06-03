// `featx` is gated here so optdep itself is drift-free; demo activates it via
// `optdep/featx` and `optdep?/featx`.
#[cfg(feature = "featx")]
pub fn featx_only() {}
