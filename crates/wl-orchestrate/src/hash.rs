//! One small, stable, non-cryptographic hash primitive shared across
//! orchestration. FNV-1a/64 is inlined rather than reached for via std's
//! `DefaultHasher` because that algorithm is unspecified across Rust releases:
//! the values it feeds — config ids ([`command`](super::command)) and the
//! extractor cache key ([`source`](super::source)) — must stay byte-stable
//! build-to-build (a config id is snapshotted; a drifting cache key would
//! silently re-materialize + rebuild the dylib on every toolchain bump).

/// The FNV-1a/64 offset basis — the seed to start a fold from.
pub(super) const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

/// Fold `bytes` into the running FNV-1a/64 state `h`. Chainable: thread the
/// return value through successive calls to hash a sequence of byte runs
/// (e.g. `(path, contents)` pairs) without allocating a joined buffer.
pub(super) fn fnv1a(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}
