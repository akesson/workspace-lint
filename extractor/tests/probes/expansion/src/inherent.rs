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

impl Carrier {
    /// By-value receiver on a non-`Copy` type — the `wrong_self_convention`
    /// shape the clippy-unmask guard replays (`self_kind: "value"`,
    /// `self_copy: false`).
    pub fn is_heavy(self) -> bool {
        self.value > 10
    }
}

/// `Copy` self type: by-value receivers are convention-CORRECT here
/// (`self_copy: true` must be emitted so the guard stays quiet).
#[derive(Clone, Copy)]
pub struct Chip(pub u32);

impl Chip {
    pub fn to_units(self) -> u32 {
        self.0
    }
}

/// Use-site shapes for `RefEdge::receiver_resolved`: `c.same_module()` is a
/// receiver-based method call (no written path — must flag `true`), while
/// `Chip::to_units(chip)` is a written `TypeRelative` path (resolves `Chip`
/// through its name — must stay `false`).
pub fn call_shapes(c: &Carrier, chip: Chip) -> (u32, u32) {
    (c.same_module(), Chip::to_units(chip))
}

/// Lexical-module probe for `RefEdge::from_module` (schema 6): a trait-impl
/// member renders at `<Type as Trait>::member` — the module the impl is
/// written in appears only inside the bracket segment, so import-scope
/// attribution needs the lexical module as its own emitted fact (the
/// LeaveDates `DateFn`/`from_iter` dangling-import breakage).
pub mod lexical {
    pub trait Yardstick {
        fn raw(&self) -> u32;
    }

    impl Yardstick for super::Chip {
        fn raw(&self) -> u32 {
            helper(self.0)
        }
    }

    pub fn helper(v: u32) -> u32 {
        v
    }
}
