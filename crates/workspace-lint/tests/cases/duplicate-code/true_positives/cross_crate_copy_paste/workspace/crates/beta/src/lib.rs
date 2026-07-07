pub fn slugify(input: &str) -> String {
    let stripped = input.trim();
    let lower = stripped.to_lowercase();
    let slug = lower.replace(' ', "_");
    slug
}
