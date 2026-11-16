// `BuildErr` has no use-site at all — its own `impl From<String>` is part of
// the type, not a user of it. But derive_builder expands
// `#[builder(build_fn(error = "…"))]` into
// `pub fn build(&self) -> Result<Widget, BuildErr>` — a generated public
// signature exposing `BuildErr` (narrowing it would trip
// `private_interfaces`). The engine sees that expansion, so the item is
// signature-exposed and stays silent.
use derive_builder::Builder;

#[derive(Builder)]
#[builder(build_fn(error = "inner::BuildErr"))]
pub struct Widget {
    size: u32,
}

impl From<String> for inner::BuildErr {
    fn from(_: String) -> Self {
        inner::BuildErr::Bad
    }
}

mod inner {
    pub enum BuildErr {
        Bad,
    }
}
