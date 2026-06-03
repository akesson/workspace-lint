pub mod api {
    pub struct User;
    // Re-export an internal type from the public surface.
    pub use crate::internal::InternalUser;
}

pub mod internal {
    pub struct InternalUser;
}
