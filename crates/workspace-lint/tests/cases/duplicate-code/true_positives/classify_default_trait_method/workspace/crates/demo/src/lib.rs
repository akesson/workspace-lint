//! One trait, two impls with identical method bodies — hoist to a default
//! method on the trait.

pub struct Celsius(pub f64);
pub struct Fahrenheit(pub f64);

pub trait Scale {
    fn fingerprint(&self) -> u32;
}

impl Scale for Celsius {
    fn fingerprint(&self) -> u32 {
        let mut acc = 0u32;
        acc = acc.wrapping_add(1);
        acc = acc.rotate_left(3);
        acc.swap_bytes().count_ones()
    }
}

impl Scale for Fahrenheit {
    fn fingerprint(&self) -> u32 {
        let mut acc = 0u32;
        acc = acc.wrapping_add(1);
        acc = acc.rotate_left(3);
        acc.swap_bytes().count_ones()
    }
}
