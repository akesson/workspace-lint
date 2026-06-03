pub fn run() {
    // `extra` is gated only here, inside a fn body. feature-drift v1 doesn't
    // descend into bodies, so this gate is invisible and `extra` is wrongly
    // reported as declared-never-gated.
    #[cfg(feature = "extra")]
    let _x = 1;
}
