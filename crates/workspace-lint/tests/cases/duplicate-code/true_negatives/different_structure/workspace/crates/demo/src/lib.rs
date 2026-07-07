pub fn scale_then_shift(values: &[u32]) -> u32 {
    let mut acc = 0u32;
    for value in values {
        acc = acc.wrapping_mul(*value);
        acc = acc.rotate_left(3);
    }
    acc ^ 0xdead
}

pub fn shift_then_scale(items: &[u32]) -> u32 {
    let mut state = 0u32;
    for item in items {
        state = state.rotate_left(3);
        state = state.wrapping_mul(*item);
    }
    state ^ 0xdead
}
