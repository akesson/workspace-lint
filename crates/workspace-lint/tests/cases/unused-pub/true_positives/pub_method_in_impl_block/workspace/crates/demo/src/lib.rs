// TRUE POSITIVE (unused-pub) — the rustc engine's flip of the old syn-era
// known false negative: `Foo::never_used` is a genuinely-unused `pub` method
// on an internal (publish-absent) crate. The syn resolver never enumerated
// items inside `impl` blocks, so the method was structurally invisible; the
// engine's inherent-impl candidates make it fire "appears unused".

pub struct Foo;

impl Foo {
    pub fn never_used(&self) {}
}

// Private anchor so `Foo` itself is referenced intra-crate (suppress-intra-crate
// drops that finding), leaving the impl method as the sole missed finding.
fn _anchor() -> Foo {
    Foo
}
