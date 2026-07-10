// `Meta` is referenced only inside this crate (IntraCrate), but it is the type
// of the public `meta` field of the public `Wrapper` struct — a public-
// signature position. Tightening it would trip `private_interfaces` (E0446)
// on the fixed tree.
mod inner {
    pub struct Meta;
}

pub struct Wrapper {
    pub meta: inner::Meta,
}
