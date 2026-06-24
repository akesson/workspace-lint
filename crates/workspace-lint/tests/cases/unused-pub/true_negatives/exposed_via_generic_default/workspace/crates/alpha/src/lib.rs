// `DefaultArg` is referenced only inside this crate (so the resolver would class
// it IntraCrate), but it is the default type argument of the public `Defaulted`
// struct — a public-signature position. The signature walk records generic-
// parameter defaults so the guard suppresses the tighten; without it, `--fix`
// would narrow `DefaultArg` to `pub(crate)` and trip `private_interfaces`.
mod inner {
    pub struct DefaultArg;
}

pub struct Defaulted<T = inner::DefaultArg>(std::marker::PhantomData<T>);
