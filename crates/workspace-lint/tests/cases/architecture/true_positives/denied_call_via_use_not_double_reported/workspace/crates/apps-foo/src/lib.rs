// The denied item is imported once and then referenced three times. The import
// fires (anchored at the `use` line); the three fully-qualified references must
// NOT add further diagnostics — a violation reported via its `use` binding is
// deduped against the code-reference pass. Exactly one diagnostic total.
use data_models::internal::InternalUser;

pub fn a() -> InternalUser {
    InternalUser
}

pub fn b() -> InternalUser {
    InternalUser
}

pub fn c() -> InternalUser {
    InternalUser
}
