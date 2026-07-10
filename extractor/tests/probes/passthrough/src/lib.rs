//! See Cargo.toml — the macro deliberately re-emits `$it` verbatim so the
//! expansion output is the caller's own root-context tokens (the cfg_if
//! shape), leaving no expansion chain for `record_macro_expansion` to walk.

#[macro_export]
macro_rules! passthrough {
    ($($it:item)*) => { $($it)* };
}
