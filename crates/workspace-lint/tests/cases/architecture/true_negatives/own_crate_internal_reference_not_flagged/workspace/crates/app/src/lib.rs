// This crate references its own `internal` module via all three inspected forms.
// Each canonical is rooted in `app` (this crate), so the cross-crate-only guard
// must exempt them — a wildcard `*::internal::**` deny is about OTHER crates'
// internals, never a crate's use of its own.
pub mod internal {
    pub struct Helper;
    impl Helper {
        pub fn new() -> Self {
            Helper
        }
    }
}

// `use` binding form.
mod via_use {
    use crate::internal::Helper;
    pub fn make() -> Helper {
        Helper::new()
    }
}

// Glob-import form.
mod via_glob {
    use crate::internal::*;
    pub fn make() -> Helper {
        Helper::new()
    }
}

// Fully-qualified-call form (no `use`).
pub fn via_fully_qualified() -> crate::internal::Helper {
    crate::internal::Helper::new()
}
