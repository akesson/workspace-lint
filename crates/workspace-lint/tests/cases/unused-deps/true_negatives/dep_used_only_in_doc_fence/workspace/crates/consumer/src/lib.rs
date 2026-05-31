//! A crate whose only reference to its `provider` dependency lives in a
//! doc-test example. The dep is genuinely used — the doc-test won't compile
//! without it — so `unused-deps` must not flag it.

/// Demonstrates the provider:
///
/// ```
/// use provider::shared;
/// shared();
/// ```
pub fn run() {}
