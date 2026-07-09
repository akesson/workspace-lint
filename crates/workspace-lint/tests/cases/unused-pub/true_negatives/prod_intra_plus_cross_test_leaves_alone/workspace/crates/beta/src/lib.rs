//! `beta` reaches `alpha::helper` only from its `#[cfg(test)]` module — but
//! alpha's own production code also calls it, so the item is neither dead
//! nor tightenable (`pub(crate)` cannot reach this test crate).

#[cfg(test)]
mod tests {
    #[test]
    fn calls_alpha_helper() {
        assert_eq!(alpha::helper(), 1);
    }
}
