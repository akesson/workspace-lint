// `Thing` is reachable as part of this PUBLISHED crate's API only through the
// glob re-export below — its own module `inner` is private, and nothing
// references `Thing` directly. External reachability sees through the glob,
// so `Thing` is exempt rather than flagged "appears unused". (regex's
// `Locations` false-positive class.) The workspace-internal twin is
// `true_positives/glob_reexported_item_of_internal_crate_flagged`.
mod inner;

pub use inner::*;
