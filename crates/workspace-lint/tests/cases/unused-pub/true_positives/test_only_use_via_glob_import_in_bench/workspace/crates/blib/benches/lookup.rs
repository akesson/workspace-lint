// The bench links `blib` as an external crate and reaches its items only
// through a glob import: `alpha` as a bare name, `helpers::beta` as a
// multi-segment run rooted at a glob-imported module. Both must read as
// sibling-target references — the items must stay `pub` (a bench can't see
// `pub(crate)`), so no finding fires even without suppress-intra-crate.
use blib::*;

fn main() {
    let _ = alpha();
    let _ = helpers::beta();
}
