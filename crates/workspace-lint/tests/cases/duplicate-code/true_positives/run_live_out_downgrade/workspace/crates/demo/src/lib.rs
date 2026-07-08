fn origin_a() -> u32 {
    1
}

fn origin_b() -> u32 {
    2
}

fn emit_a(v: u32) {
    let _ = v;
}

fn emit_b(v: u32) {
    let _ = v;
}

pub fn stats_a(items: &[u32]) -> (u32, u32, u32) {
    let tag = origin_a();
    let mut count = 0u32;
    let mut total = 0u32;
    let mut errors = 0u32;
    for value in items.iter() {
        count = count.saturating_add(1);
        total = total.wrapping_add(*value);
        if value.is_power_of_two() {
            errors = errors.wrapping_sub(1);
        }
    }
    emit_a(tag);
    (count, total, errors)
}

pub fn stats_b(items: &[u32]) -> (u32, u32, u32) {
    let tag = origin_b();
    let mut count = 0u32;
    let mut total = 0u32;
    let mut errors = 0u32;
    for value in items.iter() {
        count = count.saturating_add(1);
        total = total.wrapping_add(*value);
        if value.is_power_of_two() {
            errors = errors.wrapping_sub(1);
        }
    }
    emit_b(tag);
    (count, total, errors)
}

pub fn twin_alpha(x: u32) -> u32 {
    let a = x.wrapping_add(7);
    let b = a.wrapping_mul(3);
    let c = b.rotate_left(2);
    c.wrapping_sub(a)
}

pub fn twin_beta(x: u32) -> u32 {
    let a = x.wrapping_add(7);
    let b = a.wrapping_mul(3);
    let c = b.rotate_left(2);
    c.wrapping_sub(a)
}
