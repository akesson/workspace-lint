// A glob import of a denied module is just as much a violation as naming the
// item directly — it must not be a silent bypass of the architecture rule.
use data_models::internal::*;

pub fn touch() -> InternalUser {
    InternalUser
}
