use data_models::api::User;
use data_models::internal::InternalUser;

pub fn process_user() -> User {
    let _hidden_dependency_on_internals = InternalUser;
    User
}
