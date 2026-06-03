// PublicToken is in the deny subtree but listed as an exception, so this
// import is allowed.
use data_models::internal::PublicToken;

pub fn touch() -> PublicToken {
    PublicToken
}
