// `BuildErr` is referenced only inside this crate (via the `impl From<String>`
// self-type that derive_builder requires), so the resolver would class it
// IntraCrate and tighten it to `pub(crate)`. But derive_builder expands
// `#[builder(build_fn(error = "…"))]` into
// `pub fn build(&self) -> Result<Widget, BuildErr>` — a generated public
// signature exposing `BuildErr`. The builder-attr signature-exposure guard
// suppresses it.
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
