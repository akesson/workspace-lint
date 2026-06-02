// KNOWN FALSE POSITIVE: this function is exported for the C ABI and consumed
// by a linker / non-Rust caller, so it has no Rust referrer in the workspace.
// The resolver doesn't capture item attributes, so `#[no_mangle]` can't exempt
// it — unused-pub flags it. If attribute capture lands and exempts FFI exports,
// this case stops firing and should be reclassified.
#[unsafe(no_mangle)]
pub extern "C" fn exported_for_ffi() -> i32 {
    42
}
