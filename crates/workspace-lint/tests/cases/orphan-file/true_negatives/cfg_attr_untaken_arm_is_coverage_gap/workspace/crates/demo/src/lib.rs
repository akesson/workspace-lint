// TRUE NEGATIVE (orphan-file) — only the `not(test)` arm compiles under this
// matrix, so nothing ever opens `imp_test.rs`. The fast tier still NAMES it, so
// the lint must NOT call it an orphan: it emits a non-failing coverage-gap
// warning instead. Accusing it here is the exact false positive this lint used
// to produce against `memmap2`-style platform modules. The snapshot pins the
// gap's wording; the `deny` level above proves the gap cannot fail a build.
#[cfg_attr(test, path = "imp_test.rs")]
#[cfg_attr(not(test), path = "imp_main.rs")]
mod imp;

pub fn go() -> u32 {
    imp::val()
}
