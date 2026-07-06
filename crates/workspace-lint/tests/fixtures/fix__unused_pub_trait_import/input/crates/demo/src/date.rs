use crate::datefn::DateFn;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Date(pub u32);

impl DateFn for Date {
    fn raw(&self) -> u32 {
        self.0
    }
}
