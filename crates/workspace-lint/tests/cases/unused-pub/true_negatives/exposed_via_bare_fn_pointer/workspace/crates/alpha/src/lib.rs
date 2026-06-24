// `Arg` and `Ret` are referenced only inside this crate (so the resolver would
// class them IntraCrate), but both appear in the bare-fn-pointer type
// `fn(Arg) -> Ret` that an exempt `pub fn` returns. Tightening either to
// `pub(crate)` would make a `pub fn` expose a less-public type — rejected by
// `private_interfaces`. The signature walk's `BareFn` arm must record both so
// the guard suppresses them.
mod inner {
    pub struct Arg;
    pub struct Ret;
}

pub fn callback() -> fn(inner::Arg) -> inner::Ret {
    |_| inner::Ret
}
