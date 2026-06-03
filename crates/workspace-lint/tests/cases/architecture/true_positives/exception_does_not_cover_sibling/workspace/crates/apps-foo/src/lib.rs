// Only PublicToken is excepted; InternalUser is a sibling in the same denied
// subtree and must still fire.
use data_models::internal::InternalUser;

pub fn touch() -> InternalUser {
    InternalUser
}
