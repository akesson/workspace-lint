// `generated.rs` is written by `build.rs` during the compile and is absent
// from the checkout. Its items must stay unjudged exactly as when the file is
// checked in (the sibling `generated_anchored_not_reported` case);
// `real_unused` (handwritten, genuinely unused) must still fire.
include!("generated.rs");

pub fn real_unused() {
    gen_intra();
}
