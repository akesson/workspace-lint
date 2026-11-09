use std::fmt;

/// Inherent impl only.
pub struct DeadInherent;

impl DeadInherent {
    pub fn describe(&self) -> &'static str {
        "inherent"
    }
}

/// Hand-written trait impl only.
pub struct DeadTraitImpl;

impl fmt::Display for DeadTraitImpl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("trait")
    }
}

/// Derives only.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DeadDerived {
    pub x: u32,
}

/// Constructed only inside its own impl (`Self` / `DeadSelfBuilt { .. }`).
pub enum DeadSelfBuilt {
    A,
}

impl DeadSelfBuilt {
    pub fn make() -> Self {
        DeadSelfBuilt::A
    }
}
