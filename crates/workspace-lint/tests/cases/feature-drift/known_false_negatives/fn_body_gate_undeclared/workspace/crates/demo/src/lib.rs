pub fn run() {
    // Body-level gate referencing the undeclared feature `undeclared_feat`.
    // feature-drift v1 doesn't descend into bodies, so it never fires the
    // `gated_undeclared` it should.
    #[cfg(feature = "undeclared_feat")]
    let _x = 1;
}
