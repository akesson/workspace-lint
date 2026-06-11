use demo::add;

#[test]
fn adds_small_numbers() {
    assert_eq!(add(2, 3), 5);
}

#[test]
fn adds_zero() {
    assert_eq!(add(0, 7), 7);
}
