// `NoticeErr` is referenced only inside this crate (via the `impl From`
// self-type), so the resolver would class it IntraCrate and tighten it to
// `pub(crate)`. But typed_builder expands `#[builder(build_method(into = …))]`
// into `pub fn build(self) -> Result<Notice, NoticeErr>` — a generated public
// signature that exposes `NoticeErr`. Narrowing it would trip
// `private_interfaces` (and E0446 in the trait-impl variant). The builder-attr
// signature-exposure guard suppresses it.
use typed_builder::TypedBuilder;

#[derive(TypedBuilder)]
#[builder(build_method(into = Result<Notice, inner::NoticeErr>))]
pub struct Notice {
    pub text: String,
}

impl From<Notice> for Result<Notice, inner::NoticeErr> {
    fn from(n: Notice) -> Self {
        Ok(n)
    }
}

mod inner {
    pub enum NoticeErr {
        Empty,
    }
}
