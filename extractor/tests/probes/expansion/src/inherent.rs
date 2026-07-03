//! Inherent-impl probes for `ItemFact::self_type` (the external-reachability
//! handle from a method to its nominal self type). The `remote_impl` case is
//! the load-bearing one: `def_path_str` renders an impl block at the *impl's*
//! module (`…::remote_impl::<impl inherent::Carrier>::remote_method`), so no
//! path-prefix lookup can recover the type — only the emitted key link can.

/// The type; one impl here, one in the `remote_impl` module below.
pub struct Carrier {
    pub value: u32,
}

impl Carrier {
    /// Same-module inherent method.
    pub fn same_module(&self) -> u32 {
        self.value
    }
}

pub mod remote_impl {
    impl super::Carrier {
        /// Remote-module inherent method — the dogfood case that forced the
        /// emitted key link (`syn-workspace`'s `impl Workspace` in queries.rs).
        pub fn remote_method(&self) -> u32 {
            self.value + 1
        }
    }
}
