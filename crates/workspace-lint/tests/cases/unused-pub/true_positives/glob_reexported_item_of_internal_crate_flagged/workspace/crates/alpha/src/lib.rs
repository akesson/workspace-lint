// `Thing` is reachable only through the glob re-export below, and nothing in
// the workspace references it. This crate is workspace-internal (no
// `publish`), so it IS unused. A glob re-export is not a must-stay-`pub`
// guard: unlike a named `pub use m::Thing` (E0364), `pub use inner::*`
// silently skips an item that is no longer visible enough, so narrowing or
// deleting `Thing` compiles. (LeaveDates `data_model::api`'s
// `pub use auth::*` hid every unused auth model.) The published twin is
// `true_negatives/used_via_glob_reexport`.
mod inner;

pub use inner::*;
