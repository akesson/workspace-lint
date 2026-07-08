pub struct Config {
    pub factor: u32,
}

fn base_offset() -> u32 {
    3
}

fn base_ceiling() -> u32 {
    7
}

fn report_summary(v: u32) {
    let _ = v;
}

fn record_total(v: u32) {
    let _ = v;
}

pub fn summarize(items: &[u32], config: &Config) -> u32 {
    let offset = base_offset();
    let mut total = 0u32;
    for value in items.iter() {
        let weighted = value.wrapping_mul(config.factor);
        total = total.wrapping_add(weighted);
    }
    report_summary(total);
    total.wrapping_sub(offset)
}

pub fn aggregate(items: &[u32], config: &Config) -> u32 {
    let offset = base_ceiling();
    let mut total = 0u32;
    for value in items.iter() {
        let weighted = value.wrapping_mul(config.factor);
        total = total.wrapping_add(weighted);
    }
    record_total(total);
    total.wrapping_sub(offset)
}
