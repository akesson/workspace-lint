// Integration tests are a separate target; architecture rules apply to the
// primary unit (lib/bin) only. A denied *fully-qualified* reference here must
// NOT fire — the same exemption the `use`-binding form gets, now proven for the
// code-reference pass too.
#[test]
fn uses_internal() {
    let _ = data_models::internal::InternalUser;
}
