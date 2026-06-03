// `--fix` end-to-end coverage for unused-pub's source rewrite.
//
// `used_intra_crate` (in helpers.rs) has an intra-crate referrer, so it is the
// `IntraCrate` class — a `MachineApplicable` tighten that `--fix` applies.
//
// `entry` is a crate-root `pub fn` with no referrer the resolver can see, so it
// is the `Unused` class. Its tighten is `MaybeIncorrect` (resolver blind spot),
// so `--fix` must leave it `pub` — that's the safety property being locked in.
pub mod helpers;

pub fn entry() {
    helpers::used_intra_crate();
}
