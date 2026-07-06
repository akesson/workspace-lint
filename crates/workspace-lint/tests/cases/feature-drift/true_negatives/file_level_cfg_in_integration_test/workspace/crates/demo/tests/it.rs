#![cfg(feature = "integration-test")]

#[test]
fn only_with_feature() {
    demo::shipped();
}
