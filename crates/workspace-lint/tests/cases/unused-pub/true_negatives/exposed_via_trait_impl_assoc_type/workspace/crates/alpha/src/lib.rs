// The E0446 shape, mirroring the real `MockJoke` failure: `Resp` is named only
// inside the crate (so the resolver classes it IntraCrate and would tighten it),
// but it is the value of a public trait-impl associated type. Narrowing it to
// `pub(crate)` is a hard compile error — `crate-private type in public
// interface` (E0446) — not a mere warning. The guard must suppress it.
pub trait Endpoint {
    type Response;
}

pub struct JokeEndpoint;

impl Endpoint for JokeEndpoint {
    type Response = inner::Resp;
}

mod inner {
    pub struct Resp;
}
