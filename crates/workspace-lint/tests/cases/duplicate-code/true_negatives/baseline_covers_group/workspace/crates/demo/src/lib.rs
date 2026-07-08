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
