use crate::datefn::DateFn;
use crate::date::Date;

pub struct List(Vec<Date>);

impl List {
}

impl FromIterator<Date> for List {
    fn from_iter<I: IntoIterator<Item = Date>>(iter: I) -> Self {
        let mut v: Vec<Date> = iter.into_iter().collect();
        v.sort_by(|a, b| a.year().cmp(&b.year()));
        Self(v)
    }
}
