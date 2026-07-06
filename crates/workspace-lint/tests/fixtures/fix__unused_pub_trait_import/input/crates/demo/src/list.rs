use crate::datefn::DateFn;
use crate::date::Date;

pub struct List(Vec<Date>);

impl List {
    /// Dead: nothing calls this. Uses the trait method through a closure,
    /// exactly like the surviving impl below.
    pub fn dead_sort(&mut self) {
        self.0.sort_by(|a, b| a.year().cmp(&b.year()));
    }
}

impl FromIterator<Date> for List {
    fn from_iter<I: IntoIterator<Item = Date>>(iter: I) -> Self {
        let mut v: Vec<Date> = iter.into_iter().collect();
        v.sort_by(|a, b| a.year().cmp(&b.year()));
        Self(v)
    }
}
