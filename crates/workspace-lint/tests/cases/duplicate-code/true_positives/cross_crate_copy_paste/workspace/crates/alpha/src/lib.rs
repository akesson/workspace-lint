pub fn normalize_label(raw: &str) -> String {
    let trimmed = raw.trim();
    let lowered = trimmed.to_lowercase();
    let cleaned = lowered.replace(' ', "-");
    cleaned
}
