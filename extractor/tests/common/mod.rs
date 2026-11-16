//! Helpers shared by the probe test binaries.

use std::path::{Path, PathBuf};

/// Locate the dylib dylint-link copied for THIS package's pinned toolchain
/// (`rust-toolchain.toml`'s `channel`). `target/` outlives pin bumps, and the
/// previous pin's `@<toolchain>` copy stays next to the new one — taking any
/// `@` match once ran a stale build under the old toolchain and faked a
/// passing probe suite for a pin bump. Mirrors the orchestrator's
/// `find_dylib` in `wl-orchestrate/src/source.rs`.
pub fn find_dylib(dir: &Path) -> anyhow::Result<PathBuf> {
    let pin = pinned_channel()?;
    let stem = format!("wl_extractor@{pin}-");
    for entry in std::fs::read_dir(dir)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let pinned = name.strip_prefix("lib").unwrap_or(&name).starts_with(&stem);
        let dylib_ext = name.ends_with(".dylib") || name.ends_with(".so") || name.ends_with(".dll");
        if pinned && dylib_ext {
            return Ok(entry.path());
        }
    }
    anyhow::bail!(
        "no wl_extractor@{pin}-<host> dylib under {} (a stale dylint-link? \
         `cargo install dylint-link --locked --force`, then remove target/)",
        dir.display()
    );
}

/// `channel = "…"` from the package's `rust-toolchain.toml`.
fn pinned_channel() -> anyhow::Result<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("rust-toolchain.toml");
    let text = std::fs::read_to_string(&path)?;
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("channel"))
        .filter_map(|rest| rest.trim_start().strip_prefix('='))
        .map(|v| v.trim().trim_matches('"').to_string())
        .next()
        .ok_or_else(|| anyhow::anyhow!("no `channel` in {}", path.display()))
}
