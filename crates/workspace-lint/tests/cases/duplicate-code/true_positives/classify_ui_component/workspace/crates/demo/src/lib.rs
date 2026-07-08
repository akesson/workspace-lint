//! Two component fns built from the same `rsx!` markup — extract one component.
//! Non-identical (the label text differs) so the merge family defers to this.

macro_rules! rsx {
    ($($t:tt)*) => {
        0u32
    };
}

pub fn panel_one(count: u32) -> u32 {
    let total = count.wrapping_add(1).rotate_left(3);
    rsx! { section { class: "panel", "one" } footer { total } }
}

pub fn panel_two(count: u32) -> u32 {
    let total = count.wrapping_add(1).rotate_left(3);
    rsx! { section { class: "panel", "two" } footer { total } }
}
