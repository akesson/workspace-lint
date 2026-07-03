//! Two shapes on purpose: `Duration` is defined in core/std (a use through
//! us must record `via: "shim"`), `OwnItem` is defined here (the written
//! root IS the defining crate — `via` must stay `None`).
pub use std::time::Duration;

pub struct OwnItem;
