pub fn checksum(values: &[u32]) -> u32 {
    let mut acc = 0u32;
    for value in values {
        acc = acc.wrapping_add(*value);
        acc = acc.rotate_left(3);
    }
    acc ^ 0xdead
}

pub fn digest(items: &[u32]) -> u32 {
    let mut state = 0u32;
    for item in items {
        state = state.wrapping_add(*item);
        state = state.rotate_left(7);
    }
    state ^ 0xbeef
}
