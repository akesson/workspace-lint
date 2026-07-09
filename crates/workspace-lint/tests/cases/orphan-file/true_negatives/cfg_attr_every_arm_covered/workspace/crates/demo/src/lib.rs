// TRUE NEGATIVE (orphan-file) — the multi-arm `#[cfg_attr(.., path = ..)]` form
// (the shape `memmap2`, `socket2`, and `tempfile` use for platform modules).
// Each arm names a real file; the matrix compiles both, so both are live.
#[cfg_attr(test, path = "imp_test.rs")]
#[cfg_attr(not(test), path = "imp_main.rs")]
mod imp;

pub fn go() -> u32 {
    imp::val()
}
