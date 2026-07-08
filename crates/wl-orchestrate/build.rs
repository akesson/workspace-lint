//! Vendors the extractor package (and its `wl-ir` path dependency) into the
//! binary, and surfaces the extractor's toolchain pin as a compile-time env.
//!
//! Why vendoring (migration decision, plan `two-decisions-drop-ancient-
//! snowflake`): the IR schema, the assembler, and the extractor dylib must
//! agree in lockstep — embedding the sources at compile time makes version
//! skew structurally impossible, and a dev/CI build automatically carries the
//! in-repo extractor. At runtime `orchestrate::source` materializes these
//! files into a per-binary-version cache dir, preserving the repo-relative
//! layout (`extractor/` + `crates/wl-ir/`) so the extractor's
//! `path = "../crates/wl-ir"` dependency resolves unchanged.
//!
//! The file set is a hand-maintained closed list — the extractor is a
//! single-file cdylib by design. `tests/` are deliberately not shipped.

use std::io::Write;
use std::path::Path;

/// Repo-relative paths (also the materialized layout) of every vendored file.
const VENDORED: &[&str] = &[
    "extractor/Cargo.toml",
    "extractor/Cargo.lock",
    "extractor/rust-toolchain.toml",
    "extractor/.cargo/config.toml",
    "extractor/src/lib.rs",
    "crates/wl-ir/Cargo.toml",
    "crates/wl-ir/src/lib.rs",
];

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let repo_root = Path::new(&manifest_dir).join("../..");
    let repo_root = repo_root.canonicalize().unwrap_or(repo_root);

    let out = std::env::var("OUT_DIR").unwrap();
    let mut vendored = std::fs::File::create(Path::new(&out).join("vendored.rs")).unwrap();
    writeln!(
        vendored,
        "/// (repo-relative path, contents) of every file the extractor build needs."
    )
    .unwrap();
    writeln!(vendored, "pub static VENDORED_FILES: &[(&str, &[u8])] = &[").unwrap();
    for rel in VENDORED {
        let abs = repo_root.join(rel);
        assert!(
            abs.exists(),
            "vendored extractor file missing: {} — update VENDORED in wl-orchestrate/build.rs",
            abs.display()
        );
        println!("cargo::rerun-if-changed={}", abs.display());
        writeln!(
            vendored,
            "    ({rel:?}, include_bytes!({:?})),",
            abs.display()
        )
        .unwrap();
    }
    writeln!(vendored, "];").unwrap();

    // The single source of truth for the extractor's nightly pin. Parsed here
    // (plain string scan — no toml dep for one field) and exposed to the
    // preflight check + error messages as WL_EXTRACTOR_TOOLCHAIN.
    let toolchain_file = repo_root.join("extractor/rust-toolchain.toml");
    let text = std::fs::read_to_string(&toolchain_file).unwrap();
    let channel = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("channel = \""))
        .and_then(|rest| rest.split('"').next())
        .expect("extractor/rust-toolchain.toml must pin `channel = \"nightly-…\"`");
    println!("cargo::rustc-env=WL_EXTRACTOR_TOOLCHAIN={channel}");
}
