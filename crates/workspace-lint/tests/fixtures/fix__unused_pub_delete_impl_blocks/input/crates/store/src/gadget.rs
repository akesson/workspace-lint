/// Dead, re-exported through `pub use gadget::Gadget` — deleted with its
/// impl and the re-export.
pub struct Gadget<T: Copy> {
    pub value: T,
}

impl<T: Copy> Gadget<T> {
    pub fn get(&self) -> T {
        self.value
    }
}
