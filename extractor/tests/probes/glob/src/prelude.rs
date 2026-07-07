//! The glob target: a name, a macro (the `pub use` re-export idiom), a
//! derive, and a default-method trait — one representative per way a glob
//! can be load-bearing.
pub use procmacro::ProbeDerive;

pub struct Widget;

pub trait Shout {
    fn shout(&self) -> &'static str {
        "hey"
    }
}

impl Shout for Widget {}

macro_rules! widget {
    () => {
        $crate::prelude::Widget
    };
}
pub(crate) use widget;
