// The motivating true positive: names and literals differ, structure is
// identical. The bodies reference six distinct verbatim names (`u32`, `len`,
// `wrapping_add`, `rotate_left`, `swap_bytes`, `count_ones`) — a realistic
// copied pipeline that clears the default `min-distinct-anchors` floor.
pub fn checksum(values: &[u32]) -> u32 {
    let mut acc = values.len() as u32;
    for value in values {
        acc = acc.wrapping_add(*value);
        acc = acc.rotate_left(3);
        acc ^= acc.swap_bytes().count_ones();
    }
    acc ^ 0xdead
}

pub fn digest(items: &[u32]) -> u32 {
    let mut state = items.len() as u32;
    for item in items {
        state = state.wrapping_add(*item);
        state = state.rotate_left(7);
        state ^= state.swap_bytes().count_ones();
    }
    state ^ 0xbeef
}
