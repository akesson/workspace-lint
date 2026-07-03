// `keep` is referenced, so it must survive --fix. `unused_stub` (single line),
// `serde` (multi-line inline table), and `unused_block`
// (`[dependencies.unused_block]` table block) are unreferenced, so all three
// must be deleted.
pub fn hello() {
    let _ = keep::thing();
}
