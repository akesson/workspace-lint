/// Common functionality for date-like types.
pub(crate) trait DateFn: Copy {
    fn raw(&self) -> u32;

    /// Provided method — default body in the trait, never overridden, so a
    /// call resolves to the trait's own def.
    fn year(&self) -> u32 {
        self.raw() / 1000
    }
}
