// `unused_stub` is unreferenced — the finding fires, but this fixture dirties
// the manifest post-commit, so the deletion is WITHHELD and the line stays.
pub fn hello() {}
