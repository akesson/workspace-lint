// `foo` is relocated via #[path]; the target exists, so neither an orphan nor a
// broken-mod-decl should fire.
#[path = "weird/loc.rs"]
mod foo;

pub fn run() {
    foo::work();
}
