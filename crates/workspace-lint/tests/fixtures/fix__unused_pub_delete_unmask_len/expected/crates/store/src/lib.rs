pub struct Buf {
    items: Vec<u8>,
}

impl Buf {
    pub fn new() -> Self {
        Self { items: Vec::new() }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Unused, but deleting it would trip clippy `len_without_is_empty` on
    /// the surviving `len`. The unmask veto keeps it.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}
