//! Helpers shared by the probe test binaries.

use std::path::{Path, PathBuf};

/// Locate the dylib dylint-link copied for the toolchain THIS build ran
/// under. dylint-link names the copy `wl_extractor@$RUSTUP_TOOLCHAIN` (full
/// name incl. host: `libwl_extractor@nightly-2026-08-20-aarch64-apple-darwin.dylib`),
/// and rustup exports that same name to the test process — so the match is
/// exact, whatever chose the toolchain (`rust-toolchain.toml`, `+toolchain`,
/// an override). `extractor/target` outlives pin bumps and the previous
/// pin's copy stays next to the new one; taking any `@` match once ran a
/// stale build under the old toolchain and faked a passing probe suite for
/// a pin bump. (The orchestrator's cache dir is keyed by a hash that covers
/// `rust-toolchain.toml`, so it never holds two pins and needs no such
/// match.)
pub fn find_dylib(dir: &Path) -> anyhow::Result<PathBuf> {
    let toolchain = std::env::var("RUSTUP_TOOLCHAIN")
        .map_err(|_| anyhow::anyhow!("RUSTUP_TOOLCHAIN unset: run the probes via rustup's cargo"))?;
    let stem = format!("wl_extractor@{toolchain}.");
    for entry in std::fs::read_dir(dir)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let ours = name.strip_prefix("lib").unwrap_or(&name).starts_with(&stem);
        let dylib_ext = name.ends_with(".dylib") || name.ends_with(".so") || name.ends_with(".dll");
        if ours && dylib_ext {
            return Ok(entry.path());
        }
    }
    anyhow::bail!(
        "no wl_extractor@{toolchain} dylib under {} (a stale dylint-link? \
         `cargo install dylint-link --locked --force`, then remove target/)",
        dir.display()
    );
}
