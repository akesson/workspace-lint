// `mod foo;` resolves to `foo/mod.rs` (directory module form). Must be treated
// as reachable.
mod foo;

pub fn run() {
    foo::work();
}
