// `generated.rs` stands in for build-script output checked into the source
// tree (a fluent-typed i18n module, a graphql client, …): the generator owns
// it, so unused-pub findings anchored there are not actionable and must not
// surface. `real_unused` (handwritten, genuinely unused) must still fire.
include!("generated.rs");

pub fn real_unused() {
    gen_intra();
}
