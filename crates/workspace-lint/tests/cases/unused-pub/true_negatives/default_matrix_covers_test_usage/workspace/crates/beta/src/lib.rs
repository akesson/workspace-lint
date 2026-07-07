//! `beta` uses `alpha::helper` only from its `#[cfg(test)]` code, through the
//! dev-dependency on `alpha`. Under `--tests` the test harness links alpha's
//! PLAIN rlib, so the reference carries alpha's plain-generation `DefPathHash`
//! — the def only extracted into the `default` config's IR dir. The
//! cross-config global hash join is what lets `alpha::helper` read cross-crate.

#[cfg(test)]
mod tests {
    #[test]
    fn calls_alpha_helper() {
        assert_eq!(alpha::helper(), 1);
    }
}
