// KNOWN FALSE POSITIVE: this function is exported for the C ABI and consumed
// by a linker / non-Rust caller, so it has no Rust referrer in the workspace.
// The resolver doesn't capture item attributes, so `#[no_mangle]` can't exempt
// it — unused-pub flags it. If attribute capture lands and exempts FFI exports,
// this case stops firing and should be reclassified.
//
// The diagnostic still offers a "tighten to `pub(crate)`" suggestion, but
// because this is the `Unused` class (zero referrers found = resolver blind
// spot) the suggestion is `MaybeIncorrect`, so `--fix` will NOT auto-apply it.
// That's why `expected.stderr` shows the help block but no "(run --fix to
// apply ... suggestion)" footer. See `build_tighten_suggestion` in
// src/lints/unused_pub/mod.rs.
#[unsafe(no_mangle)]
pub extern "C" fn exported_for_ffi() -> i32 {
    42
}
