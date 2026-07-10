//! `harness = false` integration test (see the `[[test]]` entry in
//! Cargo.toml): compiles under `--tests` WITHOUT rustc's `--test` flag, so
//! `cfg(test)` is off and `sess.opts.test` is false. Probe check 22 asserts
//! its fragment is still named `custom_harness+test.wlir` with
//! `target_kind == "test"` — the naming contract the completeness guard
//! relies on.

fn main() {
    // Reference into the lib so the unit records at least one edge.
    probe_expansion::plain();
}
