//! Build-script probe (§17): compiles as crate `build_script_build` through
//! the same wrapper as every unit; the extractor must emit a references-only
//! fragment keyed on the owning package (`probe_expansion@build.json`).
fn main() {
    println!("cargo:rustc-env=PROBE_BUILD={}", buildstub::build_helper());
}
