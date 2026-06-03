// Integration tests are a separate target; architecture rules apply to the
// primary unit (lib/bin) only, so this denied import must NOT fire.
use data_models::internal::InternalUser;

#[test]
fn uses_internal() {
    let _ = InternalUser;
}
