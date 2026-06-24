// Control: the guard must suppress ONLY types actually in a public signature.
// Both types live behind a private `mod`, so `publish = true` exempts the public
// fns but not these. `Exposed` is a `pub fn` return type → suppressed.
// `BodyOnly` is named only inside a function body → not a signature position, so
// it stays IntraCrate and is still flagged + tightened to `pub(crate)`.
mod inner {
    pub struct Exposed;
    pub struct BodyOnly;
}

pub fn expose() -> inner::Exposed {
    inner::Exposed
}

pub fn run() {
    let _ = inner::BodyOnly;
}
