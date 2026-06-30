// `helper` is `pub` but referenced only from generated code: `generated.rs` is
// spliced into this module via include!, and its `gen_user` calls `helper()` by
// bare name. The resolver resolves that bare call against the *including*
// module's scope, so `helper` reads as used intra-crate (suppressed) rather than
// "appears unused" — the false positive this fixture guards against.
include!("generated.rs");

pub fn helper() {}
