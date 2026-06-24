// No `use` — `InternalUser` is reached only through a fully-qualified path.
// Before fully-qualified-reference inspection this was a documented scope gap
// (it did not fire); now it does. Both the return type and the body value
// resolve to the same canonical, so they collapse to one diagnostic.
pub fn touch() -> data_models::internal::InternalUser {
    data_models::internal::InternalUser
}
