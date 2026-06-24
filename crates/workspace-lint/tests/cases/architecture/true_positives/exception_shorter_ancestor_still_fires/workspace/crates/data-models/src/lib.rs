pub mod api {
    pub struct User;
}

pub mod internal {
    pub struct Secret;
    impl Secret {
        pub fn new() -> Self {
            Secret
        }
    }
}
