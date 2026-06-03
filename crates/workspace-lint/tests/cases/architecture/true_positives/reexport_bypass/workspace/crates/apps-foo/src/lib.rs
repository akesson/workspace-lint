// Imports the re-export path `data_models::api::InternalUser`, but the resolver
// follows the `pub use` chain to the canonical `data_models::internal::InternalUser`,
// which the rule denies. The re-export must not be a bypass.
use data_models::api::InternalUser;

pub fn touch() -> InternalUser {
    InternalUser
}
