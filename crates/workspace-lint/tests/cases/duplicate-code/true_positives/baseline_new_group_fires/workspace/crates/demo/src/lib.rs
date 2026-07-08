pub fn compute(data: &[u32]) -> u32 {
    let mut acc = 0u32;
    for value in data.iter() {
        let scaled = value.wrapping_mul(3);
        acc = acc.wrapping_add(scaled);
    }
    acc.wrapping_sub(7)
}

pub fn tally(data: &[u32]) -> u32 {
    let mut acc = 0u32;
    for value in data.iter() {
        let scaled = value.wrapping_mul(3);
        acc = acc.wrapping_add(scaled);
    }
    acc.wrapping_sub(7)
}

pub fn transform(seed: u32) -> u32 {
    let base = seed.rotate_left(2);
    let mixed = base ^ seed;
    let folded = mixed.wrapping_shl(1);
    folded.count_ones()
}

pub fn reshape(seed: u32) -> u32 {
    let base = seed.rotate_left(2);
    let mixed = base ^ seed;
    let folded = mixed.wrapping_shl(1);
    folded.count_ones()
}
