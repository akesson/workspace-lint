fn seed_a() -> u32 {
    1
}

fn seed_b() -> u32 {
    2
}

fn finish_a(v: u32) {
    let _ = v;
}

fn finish_b(v: u32) {
    let _ = v;
}

pub fn shadow_a(items: &[u32]) -> u32 {
    let acc = seed_a();
    let mut count = 0u32;
    for value in items.iter() {
        let acc = value.wrapping_mul(2);
        count = count.saturating_add(acc);
    }
    finish_a(count);
    acc.wrapping_add(count)
}

pub fn shadow_b(items: &[u32]) -> u32 {
    let acc = seed_b();
    let mut count = 0u32;
    for value in items.iter() {
        let acc = value.wrapping_mul(2);
        count = count.saturating_add(acc);
    }
    finish_b(count);
    acc.wrapping_add(count)
}
