pub mod prelude {
    pub struct Widget;

    pub trait Shout {
        fn shout(&self) -> &'static str {
            "hey"
        }
    }

    impl Shout for Widget {}

    pub use crate::widget;
}

#[macro_export]
macro_rules! widget {
    () => {
        $crate::prelude::Widget
    };
}
