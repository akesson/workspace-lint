// `Bound` is referenced only inside this crate (so the resolver would class it
// IntraCrate), but it is the generic bound of the public `apply` fn — a
// public-signature position. The predicate sweep records inline bounds, so the
// guard suppresses the tighten; without it, `--fix` would narrow `Bound` to
// `pub(crate)` and trip `private_bounds` (E0445).
mod inner {
    pub trait Bound {
        fn value(&self) -> u32;
    }

    impl Bound for u32 {
        fn value(&self) -> u32 {
            *self
        }
    }
}

pub fn apply<R: inner::Bound>(r: R) -> u32 {
    r.value()
}
