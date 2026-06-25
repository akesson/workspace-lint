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

// `BuildErr` is named only inside the crate (the `impl From` self-type below),
// so the resolver classes it `IntraCrate` — pre-fix it would be tightened. But
// typed_builder expands `#[builder(build_method(into = …))]` into
// `pub fn build(self) -> Result<Cfg, BuildErr>`, so narrowing `BuildErr` would
// make a generated public fn expose a less-public type. The builder-attr
// signature-exposure guard suppresses it, so `--fix` must leave `BuildErr` (and
// `Cfg`) `pub` — the proc-macro analogue of the `Exposed` case above.
use typed_builder::TypedBuilder;

#[derive(TypedBuilder)]
#[builder(build_method(into = Result<Cfg, BuildErr>))]
pub struct Cfg {
    pub n: u32,
}

pub enum BuildErr {
    Bad,
}

impl From<Cfg> for Result<Cfg, BuildErr> {
    fn from(c: Cfg) -> Self {
        Ok(c)
    }
}
