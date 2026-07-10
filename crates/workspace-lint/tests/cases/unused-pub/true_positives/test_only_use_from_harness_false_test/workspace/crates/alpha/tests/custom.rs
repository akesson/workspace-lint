//! `harness = false` main (see the `[[test]]` entry in alpha's Cargo.toml):
//! the only reader of `alpha::helper`, so the verdict must be "only used by
//! test code" — proof that the harness-less unit's reach classifies as test
//! reach despite compiling without `cfg(test)`.

fn main() {
    assert_eq!(alpha::helper(), 1);
}
