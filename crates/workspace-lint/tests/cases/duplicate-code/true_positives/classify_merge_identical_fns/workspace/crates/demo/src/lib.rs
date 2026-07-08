//! Two byte-identical fns, both called from `driver` — the merge family
//! confirms they call the same things and names the call sites to redirect.

pub fn render_left(cells: &[u32]) -> u32 {
    let mut acc = cells.len() as u32;
    for cell in cells {
        acc = acc.wrapping_add(*cell);
        acc = acc.rotate_left(3);
    }
    acc.count_ones()
}

pub fn render_right(cells: &[u32]) -> u32 {
    let mut acc = cells.len() as u32;
    for cell in cells {
        acc = acc.wrapping_add(*cell);
        acc = acc.rotate_left(3);
    }
    acc.count_ones()
}

pub fn driver(cells: &[u32]) -> u32 {
    render_left(cells) + render_right(cells)
}
