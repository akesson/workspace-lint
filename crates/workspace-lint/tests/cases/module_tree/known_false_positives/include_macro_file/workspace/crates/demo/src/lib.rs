// `generated.rs` is pulled in via include!, not a `mod` declaration. The
// module-tree lint walks `mod` chains only, so it does not see the include!
// and reports the file as orphan. KNOWN FALSE POSITIVE.
include!("generated.rs");

pub fn run() {
    helper();
}
