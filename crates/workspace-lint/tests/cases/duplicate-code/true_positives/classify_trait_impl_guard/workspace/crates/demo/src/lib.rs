//! Issue #141: a required trait method shadowed by a same-named inherent
//! method. The trait copy has zero direct in-edges (`w.id()` binds the
//! inherent one), but deleting it is E0046 — the classifier must spare it
//! (merge advice + a note naming the trait method), never "delete it".

pub struct DepartmentWrapper {
    pub code: u32,
}

pub trait Selectable {
    fn id(&self) -> u32;
}

impl Selectable for DepartmentWrapper {
    fn id(&self) -> u32 {
        let mut acc = self.code;
        for step in 0..4u32 {
            acc = acc.wrapping_add(step);
            acc = acc.rotate_left(3);
        }
        acc.count_ones()
    }
}

impl DepartmentWrapper {
    pub fn id(&self) -> u32 {
        let mut acc = self.code;
        for step in 0..4u32 {
            acc = acc.wrapping_add(step);
            acc = acc.rotate_left(3);
        }
        acc.count_ones()
    }
}

pub fn direct_ids(w: &DepartmentWrapper) -> u32 {
    w.id() + w.id()
}
