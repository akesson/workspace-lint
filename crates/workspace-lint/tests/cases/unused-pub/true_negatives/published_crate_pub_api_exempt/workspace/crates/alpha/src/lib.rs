// Unused across the workspace, but `alpha` declares `publish = true`, so this
// is treated as external API surface and is NOT flagged.
pub fn unreferenced_function() {
    let _ = 42;
}
