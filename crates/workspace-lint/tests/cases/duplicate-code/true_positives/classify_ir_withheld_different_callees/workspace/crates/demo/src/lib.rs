//! Structurally identical fns whose local binding shadows a same-named callee,
//! so they match by tokens yet resolve DIFFERENT workspace fns — the merge is
//! withheld (the IR-confirm contract closes the shadowing loophole).

pub fn fmt_short(m: u32) -> u32 {
    let render = render(m);
    let scaled = render.wrapping_add(1);
    let rotated = scaled.rotate_left(3);
    rotated.swap_bytes().count_ones()
}

pub fn fmt_long(m: u32) -> u32 {
    let display = display(m);
    let scaled = display.wrapping_add(1);
    let rotated = scaled.rotate_left(3);
    rotated.swap_bytes().count_ones()
}

fn render(m: u32) -> u32 {
    m.wrapping_mul(2)
}

fn display(m: u32) -> u32 {
    m.wrapping_mul(3)
}
