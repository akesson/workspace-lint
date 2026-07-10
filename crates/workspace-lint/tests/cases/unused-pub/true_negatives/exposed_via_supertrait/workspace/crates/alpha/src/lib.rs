// `Base` is referenced only inside this crate (IntraCrate), but it is the
// supertrait of the public `Extended` trait — a public-signature position.
// Tightening it would trip `private_bounds` (E0445) on the fixed tree.
mod inner {
    pub trait Base {}
}

pub trait Extended: inner::Base {}
