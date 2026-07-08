use crate::datefn::DateFn;
use crate::date::Date;

pub struct List(Vec<Date>);

impl List {
    /// Surviving reader of the tuple field — keeps `.0` read after
    /// `dead_sort` is deleted (else the deletion-unmask veto would kick in,
    /// which is not this fixture's subject).
    pub fn dates(&self) -> &[Date] {
        &self.0
    }

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
