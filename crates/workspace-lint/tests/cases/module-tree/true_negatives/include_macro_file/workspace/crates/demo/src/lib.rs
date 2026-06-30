// `generated.rs` is pulled in via include!, not a `mod` declaration. The
// resolver follows the include!, splices `generated.rs` into this module, and
// records it as generated — so the module-tree lint sees the file as reached
// and does not flag it as an orphan.
include!("generated.rs");

pub fn run() {
    helper();
}
