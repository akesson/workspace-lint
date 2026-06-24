// Control: the builder-attr guard must suppress ONLY types promoted into a
// generated `build()` signature. Both types live behind a private `mod`, so
// `publish = true` exempts the public struct but not these. `PromotedErr` is
// named in `build_method(into = …)` → suppressed. `Unrelated` is named only
// inside a function body → not promoted, not a signature position, so it stays
// IntraCrate and is still flagged + tightened to `pub(crate)`.
use typed_builder::TypedBuilder;

#[derive(TypedBuilder)]
#[builder(build_method(into = Result<Cfg, inner::PromotedErr>))]
pub struct Cfg {
    pub n: u32,
}

impl From<Cfg> for Result<Cfg, inner::PromotedErr> {
    fn from(c: Cfg) -> Self {
        Ok(c)
    }
}

fn touch() {
    let _ = inner::Unrelated;
}

mod inner {
    pub enum PromotedErr {
        Bad,
    }
    pub struct Unrelated;
}
