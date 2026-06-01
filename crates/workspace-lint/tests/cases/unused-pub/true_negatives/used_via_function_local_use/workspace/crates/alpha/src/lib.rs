mod tables;

// The only reference to `tables::age::BY_NAME` is a function-local `use` of the
// module followed by a `Mod::ITEM` access — the resolver must honor `use`
// statements nested in fn bodies, not just module-level ones.
fn lookup() -> u32 {
    use crate::tables::age;
    age::BY_NAME
}
