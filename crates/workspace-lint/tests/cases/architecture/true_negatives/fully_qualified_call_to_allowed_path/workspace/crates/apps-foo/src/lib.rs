// Fully-qualified, but `api` is allowed (only `internal` is denied). The
// code-reference pass must not over-fire on a non-denied path.
pub fn touch() -> data_models::api::User {
    data_models::api::User
}
