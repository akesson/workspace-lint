//! Two identical fns; `used_copy` is called, `dead_copy` is referenced by
//! nothing — the classifier says delete the dead one rather than merge.

pub fn used_copy(cells: &[u32]) -> u32 {
    let mut acc = cells.len() as u32;
    for cell in cells {
        acc = acc.wrapping_add(*cell);
        acc = acc.rotate_left(3);
    }
    acc.count_ones()
}

fn dead_copy(cells: &[u32]) -> u32 {
    let mut acc = cells.len() as u32;
    for cell in cells {
        acc = acc.wrapping_add(*cell);
        acc = acc.rotate_left(3);
    }
    acc.count_ones()
}

pub fn driver(cells: &[u32]) -> u32 {
    used_copy(cells)
}
