//! Two free fns both taking `&Config` first — make it a method on `Config`.
//! Non-identical (one literal differs) so the merge family defers to this.

pub struct Config {
    pub scale: u32,
    pub offset: u32,
}

pub fn score_left(cfg: &Config) -> u32 {
    let base = cfg.scale.wrapping_add(cfg.offset);
    let adjusted = base.rotate_left(3);
    adjusted.swap_bytes().count_ones()
}

pub fn score_right(cfg: &Config) -> u32 {
    let base = cfg.scale.wrapping_add(cfg.offset);
    let adjusted = base.rotate_left(5);
    adjusted.swap_bytes().count_ones()
}
