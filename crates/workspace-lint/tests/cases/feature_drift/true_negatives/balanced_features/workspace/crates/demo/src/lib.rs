#[cfg(feature = "greet")]
pub fn greet() {}

#[cfg(any(feature = "greet", feature = "shout"))]
pub fn either() {}

#[cfg_attr(feature = "shout", inline)]
pub fn maybe_inlined() {}
