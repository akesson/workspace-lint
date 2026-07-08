//! Two identical fns with an argument-position `impl Trait` param, both called.
//! The `impl Into<String>` emits a fn-scoped `param`-kind reference edge; this
//! pins that the merge family excludes it (rather than being fooled into a
//! spurious withhold, since each fn's param identity differs).

pub fn render_left(label: impl Into<String>, cells: &[u32]) -> u32 {
    let _ = label.into();
    let mut acc = cells.len() as u32;
    for cell in cells {
        acc = acc.wrapping_add(*cell);
        acc = acc.rotate_left(3);
    }
    acc.count_ones()
}

pub fn render_right(label: impl Into<String>, cells: &[u32]) -> u32 {
    let _ = label.into();
    let mut acc = cells.len() as u32;
    for cell in cells {
        acc = acc.wrapping_add(*cell);
        acc = acc.rotate_left(3);
    }
    acc.count_ones()
}

pub fn driver(cells: &[u32]) -> u32 {
    render_left("a", cells) + render_right("b", cells)
}
