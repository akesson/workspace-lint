fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    std::fs::create_dir_all(format!("{dir}/gen")).unwrap();
    std::fs::write(
        format!("{dir}/gen/generated.rs"),
        "pub fn gen_unused() {}\n\npub fn gen_intra() {}\n",
    )
    .unwrap();
    println!("cargo:rerun-if-changed=build.rs");
}
