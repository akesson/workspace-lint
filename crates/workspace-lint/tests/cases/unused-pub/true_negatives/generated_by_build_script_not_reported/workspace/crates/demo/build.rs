fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    std::fs::write(
        format!("{dir}/src/generated.rs"),
        "pub fn gen_unused() {}\n\npub fn gen_intra() {}\n",
    )
    .unwrap();
    println!("cargo:rerun-if-changed=build.rs");
}
