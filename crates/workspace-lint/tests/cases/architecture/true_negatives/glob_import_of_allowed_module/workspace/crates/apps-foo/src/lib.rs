// Glob-importing the sanctioned public `api` module is fine — only
// `data-models::internal::**` is denied.
use data_models::api::*;

pub fn touch() -> User {
    User
}
