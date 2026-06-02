// The `#[path]` override points at a file that doesn't exist — a broken module
// declaration just like a bare `mod` with no backing file.
#[path = "does_not_exist.rs"]
pub mod foo;
