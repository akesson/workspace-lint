// KNOWN FALSE NEGATIVE (unused-pub).
//
// `Foo::never_used` is a genuinely-unused `pub` method on an internal
// (publish-absent) crate, so it *should* be flagged "appears unused". It isn't,
// because the resolver doesn't enumerate items inside `impl` blocks — the method
// is invisible to unused-pub. (Proof the lint is otherwise live: the same fn as a
// free `pub fn never_used()` *is* flagged.) When impl-block enumeration lands,
// this fires and the fixture promotes to a true_positive.

pub struct Foo;

impl Foo {
    pub fn never_used(&self) {}
}

// Private anchor so `Foo` itself is referenced intra-crate (suppress-intra-crate
// drops that finding), leaving the impl method as the sole missed finding.
fn _anchor() -> Foo {
    Foo
}
