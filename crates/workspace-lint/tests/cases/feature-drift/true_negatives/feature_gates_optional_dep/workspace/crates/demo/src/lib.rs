// No `#[cfg(feature = "tls")]` anywhere — `tls` only gates the optional
// `optdep` dependency, which is a legitimate use that must not be flagged.
pub fn hello() {}
