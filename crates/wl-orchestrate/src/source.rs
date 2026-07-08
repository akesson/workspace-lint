//! Where the extractor package comes from, and turning it into a dylib.
//!
//! At user sites the binary carries the extractor + `wl-ir` sources embedded
//! at compile time (`build.rs`; see the vendoring rationale there) and
//! materializes them into a cache directory keyed by the binary version. In
//! this repository's own tests the checked-out `extractor/` is used directly,
//! so dev builds always exercise the live source.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::EngineError;

include!(concat!(env!("OUT_DIR"), "/vendored.rs"));

/// Which extractor sources to build the dylib from.
#[derive(Debug, Clone)]
pub enum ExtractorSource {
    /// The vendored sources, materialized under
    /// `<cache_root>/<binary version>/` (repo-relative layout preserved so
    /// the extractor's `path = "../crates/wl-ir"` dependency resolves).
    Vendored { cache_root: PathBuf },
    /// A checked-out extractor package directory (this repo's `extractor/`).
    Repo { package_dir: PathBuf },
}

impl ExtractorSource {
    /// The production source: vendored, cached under
    /// `$WL_EXTRACTOR_CACHE` (if set) or `~/.cache/workspace-lint/extractor`.
    pub fn vendored() -> Self {
        let cache_root = std::env::var_os("WL_EXTRACTOR_CACHE")
            .map(PathBuf::from)
            .or_else(|| std::env::home_dir().map(|h| h.join(".cache/workspace-lint/extractor")))
            .unwrap_or_else(|| PathBuf::from(".workspace-lint-cache/extractor"));
        Self::Vendored { cache_root }
    }

    /// Ensure the extractor package exists on disk. Returns the package dir (the
    /// directory holding the extractor's `Cargo.toml`) and whether the vendored
    /// sources were already fresh: `true` when nothing had to be rewritten, so a
    /// dylib previously built from them is current and the rebuild can be
    /// skipped. `Repo` sources are live checkouts we never assume fresh, so it
    /// always reports `false`.
    pub(super) fn materialize(&self) -> Result<(PathBuf, bool), EngineError> {
        match self {
            Self::Repo { package_dir } => Ok((package_dir.clone(), false)),
            Self::Vendored { cache_root } => {
                // Keyed by binary version: a new release re-materializes; a
                // stale cache can never feed an old extractor to a new
                // assembler. Self-healing but **mtime-stable**: a file is
                // rewritten only when its content differs (missing/corrupt).
                // Unconditional rewrites would refresh source mtimes and
                // dirty cargo's fingerprint, relinking the cached dylib on
                // every invocation — needless work for one run, and a data
                // race for concurrent runs: relinking a dylib another
                // process's driver has loaded truncates a mapped file on
                // Unix (SIGBUS in that driver mid-compile) and fails the
                // link with a sharing violation on Windows.
                let root = cache_root.join(env!("CARGO_PKG_VERSION"));
                let mut sources_fresh = true;
                for (rel, bytes) in VENDORED_FILES {
                    let dest = root.join(rel);
                    if std::fs::read(&dest).is_ok_and(|existing| existing.as_slice() == *bytes) {
                        continue;
                    }
                    // A rewrite means the cached sources differed from this
                    // binary's embedded copy — the previously built dylib (if
                    // any) is stale, so the caller must rebuild.
                    sources_fresh = false;
                    let write = |source| EngineError::Materialize {
                        dir: dest.clone(),
                        source,
                    };
                    if let Some(parent) = dest.parent() {
                        std::fs::create_dir_all(parent).map_err(write)?;
                    }
                    std::fs::write(&dest, bytes).map_err(write)?;
                }
                Ok((root.join("extractor"), sources_fresh))
            }
        }
    }
}

/// Build the extractor dylib with the pinned toolchain and locate it.
///
/// `rustup`'s directory-scoped resolution applies the package's
/// `rust-toolchain.toml` — but only if the caller's environment doesn't
/// override it, so the rustup/cargo selection vars are scrubbed: when this
/// binary itself runs under `cargo run` / `cargo test`, the inherited
/// `RUSTUP_TOOLCHAIN`/`CARGO`/`RUSTC` would pin the child to the *caller's*
/// toolchain and the `rustc_private` build would fail on stable.
pub(super) fn build_dylib(package_dir: &Path) -> Result<PathBuf, EngineError> {
    // Output is captured, not inherited: a warm no-op build must leave the
    // caller's stderr byte-deterministic (snapshot consumers), and on failure
    // the compile errors are replayed verbatim — they ARE the diagnosis.
    let output = Command::new("cargo")
        .args(["build", "--locked"])
        .current_dir(package_dir)
        .env_remove("RUSTUP_TOOLCHAIN")
        .env_remove("CARGO")
        .env_remove("RUSTC")
        // Keep the dylib under the package dir even if the caller redirects
        // its own builds — find_dylib below and dylint's dep-info fingerprint
        // both key on this path.
        .env_remove("CARGO_TARGET_DIR")
        // The dylib is an internal pinned artifact: the invoker's build flags
        // must not reach it. Concretely, `cargo llvm-cov` (the CRAP CI job)
        // exports `-C instrument-coverage` — a dylib instrumented on the
        // *pinned nightly* emits profraw the invoker's stable `llvm-profdata`
        // may not merge. Any other inherited RUSTFLAGS would just vary the
        // fingerprint and force spurious rebuilds of a build the user never
        // asked to configure.
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("CARGO_BUILD_RUSTFLAGS")
        .env_remove("LLVM_PROFILE_FILE")
        .output()
        .map_err(|source| EngineError::Io {
            context: format!("spawning cargo build in {}", package_dir.display()),
            source,
        })?;
    if !output.status.success() {
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        return Err(EngineError::ExtractorBuild {
            dir: package_dir.to_path_buf(),
        });
    }
    find_dylib(&package_dir.join("target/debug"))
}

/// The extractor dylib already built in `package_dir`'s target dir, if present
/// — a pure directory read: no cargo spawn and no mtime mutation (the dylib's
/// mtime is the relink generation key). Returns the same path [`build_dylib`]
/// produces, so a warm run can skip the rebuild and reuse it directly.
pub(super) fn existing_dylib(package_dir: &Path) -> Option<PathBuf> {
    find_dylib(&package_dir.join("target/debug")).ok()
}

/// Locate the toolchain-suffixed dylib dylint_linting's build script produced.
/// Prefix and extension are per-OS (`libwl_extractor@….dylib`/`.so`,
/// `wl_extractor@….dll`); the extension filter keeps Windows' sibling
/// `.dll.lib`/`.pdb` artifacts out.
fn find_dylib(dir: &Path) -> Result<PathBuf, EngineError> {
    let entries = std::fs::read_dir(dir).map_err(|source| EngineError::Io {
        context: format!("reading {}", dir.display()),
        source,
    })?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let prefixed = name.starts_with("libwl_extractor@") || name.starts_with("wl_extractor@");
        let dylib_ext = name.ends_with(".dylib") || name.ends_with(".so") || name.ends_with(".dll");
        if prefixed && dylib_ext {
            return Ok(entry.path());
        }
    }
    Err(EngineError::DylibNotFound {
        dir: dir.to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendored_set_materializes_standalone() {
        let tmp = tempfile::tempdir().unwrap();
        let source = ExtractorSource::Vendored {
            cache_root: tmp.path().to_path_buf(),
        };
        let (pkg, fresh) = source.materialize().unwrap();
        assert!(
            !fresh,
            "first materialize into an empty cache writes every file"
        );
        assert!(pkg.join("Cargo.toml").is_file());
        assert!(pkg.join("Cargo.lock").is_file());
        assert!(pkg.join("rust-toolchain.toml").is_file());
        assert!(pkg.join(".cargo/config.toml").is_file());
        assert!(pkg.join("src/lib.rs").is_file());

        // The wl-ir path dep must resolve in the materialized layout…
        let ir_manifest = pkg.join("../crates/wl-ir/Cargo.toml");
        assert!(ir_manifest.is_file());
        // …and be self-contained: workspace inheritance has no root to
        // resolve against in the cache dir.
        for rel in ["../crates/wl-ir/Cargo.toml", "Cargo.toml"] {
            let text = std::fs::read_to_string(pkg.join(rel)).unwrap();
            let inherits = text
                .lines()
                .filter(|l| !l.trim_start().starts_with('#'))
                .any(|l| l.contains("workspace = true") || l.contains(".workspace = true"));
            assert!(
                !inherits,
                "{rel} must stay self-contained (no workspace inheritance); \
                 see the note in wl-ir/Cargo.toml"
            );
        }

        // Idempotent re-materialization (self-healing cache) — and
        // mtime-stable: an unchanged file must NOT be rewritten, or every
        // invocation would dirty cargo's fingerprint and relink the cached
        // dylib (racing any concurrent process that has it loaded). Pin a
        // sentinel mtime and assert it survives.
        let lib_rs = pkg.join("src/lib.rs");
        let sentinel =
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
        let f = std::fs::OpenOptions::new()
            .append(true)
            .open(&lib_rs)
            .unwrap();
        f.set_modified(sentinel).unwrap();
        drop(f);
        let (again, fresh) = source.materialize().unwrap();
        assert_eq!(pkg, again);
        assert!(
            fresh,
            "re-materialization of an unchanged cache must report sources fresh (nothing rewritten) \
             — this is the signal that gates the warm-run dylib-build skip"
        );
        assert_eq!(
            std::fs::metadata(&lib_rs).unwrap().modified().unwrap(),
            sentinel,
            "re-materialization rewrote an unchanged file — warm caches must stay mtime-stable"
        );
    }

    #[test]
    fn pinned_toolchain_is_a_dated_nightly() {
        let pin = crate::Engine::pinned_toolchain();
        assert!(
            pin.starts_with("nightly-2"),
            "expected a dated nightly pin, got {pin}"
        );
    }
}
