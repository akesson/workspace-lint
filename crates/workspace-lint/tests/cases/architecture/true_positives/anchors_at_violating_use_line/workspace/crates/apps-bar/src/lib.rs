//! Regression fixture: the violating `use` is deliberately on line 5,
//! after a few lines of allowed imports and blank space, so the
//! diagnostic anchor `--> .../lib.rs:N:1` must point at line 5 (the
//! line where `InternalUser` is named) — not at line 1 of the file.

use data_models::internal::InternalUser;

use data_models::api::User;

pub fn touch() -> (InternalUser, User) {
    (InternalUser, User)
}
