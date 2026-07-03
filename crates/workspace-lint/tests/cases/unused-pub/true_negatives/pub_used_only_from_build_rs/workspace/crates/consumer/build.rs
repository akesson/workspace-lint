fn main() {
    println!("cargo:rustc-env=TOOLS_OUT={}", tools::copy_if_changed());
}
