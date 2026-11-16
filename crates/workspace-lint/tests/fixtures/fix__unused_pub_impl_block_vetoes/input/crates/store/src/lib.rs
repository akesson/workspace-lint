mod excluded;

pub struct Shown;

impl Shown {
    pub fn gone(&self) -> u32 {
        1
    }

    #[cfg(feature = "serde")]
    pub fn to_json(&self) -> String {
        String::new()
    }
}

pub fn make_shown() -> Shown {
    Shown
}

pub struct Hidden;

impl Hidden {
    #[cfg(feature = "serde")]
    pub fn to_json(&self) -> String {
        String::new()
    }
}

pub struct Kept;
