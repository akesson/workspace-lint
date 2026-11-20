// `build.rs` writes `../gen/generated.rs` during the compile; the checkout
// lacks it (fluent-typed's gitignored `gen/i18n.rs`: outside `src/`, via `..`).
// Its items stay unjudged as if checked in (sibling `generated_anchored_…`);
// `real_unused` (handwritten, genuinely unused) must still fire.
include!("../gen/generated.rs");

pub fn real_unused() {
    gen_intra();
}
