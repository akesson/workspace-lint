// `Thing` is reachable as part of the crate's public API only through the glob
// re-export below — its own module `inner` is private, and nothing references
// `Thing` directly. The resolver must treat a `pub use M::*` glob target as a
// re-export target (like a named `pub use`), so `Thing` is exempt rather than
// flagged "appears unused". (regex's `Locations` false-positive class.)
mod inner;

pub use inner::*;
