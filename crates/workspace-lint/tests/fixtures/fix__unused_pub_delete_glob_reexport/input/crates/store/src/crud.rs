/// Dead, but the last public items behind `pub use crud::*` — kept.
pub enum CreationError {
    Conflict,
}

/// Dead, likewise kept.
pub enum DeletionError {
    Missing,
}
