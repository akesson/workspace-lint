// Unused across the workspace and `alpha` isn't published → flagged, and (with
// publish-hint-threshold = 1) the crate-level publish hint fires.
pub fn unused_one() {
    let _ = 42;
}
