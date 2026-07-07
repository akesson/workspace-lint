pub fn ordered(base: u32, step: u32) -> u32 {
    let first = base.wrapping_mul(31);
    let second = step.wrapping_add(17);
    let mixed = first ^ second;
    mixed.rotate_left(first % 7)
}

pub fn swapped(base: u32, step: u32) -> u32 {
    let first = base.wrapping_mul(31);
    let second = step.wrapping_add(17);
    let mixed = second ^ first;
    mixed.rotate_left(second % 7)
}
