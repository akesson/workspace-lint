//! `beta` uses `alpha::helper` from production code AND from its test module:
//! the production edge must keep the item off every unused-pub verdict.
//! The caller is private (and `allow(dead_code)`) so the fixture adds no
//! unused-pub candidate of its own — only the production *edge* matters.

#[allow(dead_code)]
fn double() -> u32 {
    alpha::helper() * 2
}

#[cfg(test)]
mod tests {
    #[test]
    fn calls_alpha_helper() {
        assert_eq!(alpha::helper(), 1);
    }
}
