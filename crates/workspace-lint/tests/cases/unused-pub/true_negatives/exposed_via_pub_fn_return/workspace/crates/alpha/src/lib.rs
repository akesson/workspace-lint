// `Direct` and `Nested` are referenced only inside this crate (so the resolver
// would class them IntraCrate), but each appears in the public *signature* of an
// exempt `pub fn`. Tightening either to `pub(crate)` would make a `pub fn`
// expose a less-public type — rejected by `private_interfaces`. The
// signature-exposure guard must suppress both, including the nested-generic one.
mod inner {
    pub struct Direct;
    pub struct Nested;
}

pub fn ret() -> inner::Direct {
    inner::Direct
}

pub fn list() -> Vec<inner::Nested> {
    Vec::new()
}
