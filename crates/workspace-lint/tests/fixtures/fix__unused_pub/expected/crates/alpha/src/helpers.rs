pub(crate) fn used_intra_crate() {
    let _ = 42;
}

// `Exposed` is named only inside the crate, so the resolver classes it
// `IntraCrate` — pre-fix it would be tightened to `pub(crate)`. But it is the
// return type of the public `exposes` below, so narrowing it would make a
// `pub fn` expose a less-public type (`private_interfaces`). The
// signature-exposure guard suppresses the finding, so `--fix` must leave
// `Exposed` (and `exposes`) `pub` — proving `--fix` no longer breaks builds.
pub struct Exposed;

pub fn exposes() -> Exposed {
    Exposed
}
