// `Producer` is referenced only inside this crate (IntraCrate), but it bounds
// the public `produce` fn's opaque return type — a public-signature position
// that lives in the opaque's item bounds, not in `fn_sig`'s type tree.
// Tightening it would trip `private_bounds` on the fixed tree.
mod inner {
    pub trait Producer {}

    impl Producer for u32 {}
}

pub fn produce() -> impl inner::Producer {
    7u32
}
