#[cfg(any(test, feature = "extra"))]
pub fn maybe_a() {}
#[cfg(any(test, feature = "extra"))]
pub fn maybe_b() {}
pub fn always() {}
