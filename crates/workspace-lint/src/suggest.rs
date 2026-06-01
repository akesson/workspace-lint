//! "Did you mean …?" suggestions for typo'd config keys and lint names.

/// The closest candidate to `target` within a small edit distance, for a
/// "did you mean …?" hint. Returns `None` if nothing is close enough, so a
/// wildly different input gets no misleading suggestion.
pub(crate) fn closest<'a>(target: &str, candidates: &[&'a str]) -> Option<&'a str> {
    candidates
        .iter()
        .map(|c| (levenshtein(target, c), *c))
        .filter(|(d, _)| *d <= 3)
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c)
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_close_match() {
        assert_eq!(
            closest("unused-dep", &["unused-deps", "file-size"]),
            Some("unused-deps")
        );
        assert_eq!(
            closest("file_size", &["file-size", "crate-size"]),
            Some("file-size")
        );
    }

    #[test]
    fn no_match_when_too_far() {
        assert_eq!(closest("zzzzzzzz", &["file-size", "unused-deps"]), None);
    }
}
