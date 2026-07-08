//! Where the extractor package comes from, and turning it into a dylib.
//!
//! At user sites the binary carries the extractor + `wl-ir` sources embedded
//! at compile time (`build.rs`; see the vendoring rationale there) and
//! materializes them into a cache directory keyed by a content hash of those
//! embedded sources. In this repository's own tests the checked-out
//! `extractor/` is used directly, so dev builds always exercise the live
//! source.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use super::{EngineError, hash};

include!(concat!(env!("OUT_DIR"), "/vendored.rs"));

/// Which extractor sources to build the dylib from.
#[derive(Debug, Clone)]
pub enum ExtractorSource {
    /// The vendored sources, materialized under
    /// `<cache_root>/<source hash>/` (repo-relative layout preserved so
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
                // Content-addressed: the dir is keyed by a hash of the exact
                // embedded sources, so two binaries carrying *different*
                // vendored trees (dev builds off different branches, a pinned
                // release, a fleet-worktree install) land in disjoint dirs and
                // can never rewrite or relink each other's dylib. That
                // cross-binary in-place mutation was the poisoning bug — it
                // truncated a mapped dylib (SIGBUS, or a macOS "Code Signature
                // Invalid" SIGKILL) in a concurrent driver. A changed source
                // tree now re-materializes into a fresh dir automatically; a
                // stale cache can never feed an old extractor to a new
                // assembler.
                //
                // Within one hash dir every sharer is byte-identical, so the
                // remaining self-heal is a pure corruption check — and still
                // **mtime-stable**: a file is rewritten only when its content
                // differs (missing/corrupt), never unconditionally, because a
                // fresh mtime would dirty cargo's fingerprint and relink the
                // dylib on every run (racing any same-source concurrent run
                // that has it loaded).
                let key = cache_key(VENDORED_FILES);
                let root = cache_root.join(&key);
                std::fs::create_dir_all(&root).map_err(|source| EngineError::Materialize {
                    dir: root.clone(),
                    source,
                })?;
                // Record "used now" (warm runs touch nothing else) and reap
                // long-idle sibling variants. Best-effort and deletion-only,
                // hence safe against a concurrent run of another variant:
                // unlinking a mapped dylib preserves its inode on Unix, and
                // Windows refuses to delete a loaded image (the sweep just
                // skips it). Only *in-place mutation* — which content
                // addressing now prevents across variants — could corrupt a
                // live driver.
                touch_marker(&root);
                evict_stale(cache_root, &key, SystemTime::now(), EVICT_AFTER);
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

/// The cache dir key: FNV-1a/64 over every embedded source file, 16 hex chars.
/// Each `(rel-path, bytes)` pair is folded with `0x00` separators so no byte
/// can drift across the path/content or the file boundary (`("a", "bc")` and
/// `("ab", "c")` must hash differently). Collision-safe for the handful of
/// distinct source trees a machine ever holds, and the inputs are our own
/// files, so non-cryptographic FNV suffices (see [`hash`]).
fn cache_key(files: &[(&str, &[u8])]) -> String {
    let mut h = hash::FNV_OFFSET;
    for (rel, bytes) in files {
        h = hash::fnv1a(h, rel.as_bytes());
        h = hash::fnv1a(h, &[0]);
        h = hash::fnv1a(h, bytes);
        h = hash::fnv1a(h, &[0]);
    }
    format!("{h:016x}")
}

/// Basename of the per-run recency marker at a hash dir's root. It lives
/// *outside* the `extractor/` package, so cargo never fingerprints it and
/// touching it can't trigger a rebuild or relink.
const LAST_USED: &str = ".last-used";

/// How long a sibling hash dir must sit unused before [`evict_stale`] reaps it.
/// Generous on purpose: the only cost of reaping a variant that later returns
/// is one rebuild, and no lint run holds a dylib mapped for anything near this.
const EVICT_AFTER: Duration = Duration::from_secs(14 * 24 * 60 * 60);

/// Stamp the hash dir's recency marker to now. Best-effort: a dir that fails to
/// mark is simply never a reap candidate (a missing marker reads as
/// in-creation), so it leaks rather than risking anything.
fn touch_marker(root: &Path) {
    // `write` sets mtime to now; the (empty) payload is irrelevant.
    let _ = std::fs::write(root.join(LAST_USED), b"");
}

/// Reap sibling hash dirs idle longer than `ttl`. Best-effort, silent, and
/// deletion-only (the call site documents why deletion is safe against a
/// concurrent variant). Skips the current dir (`keep`), non-directories, and
/// any dir without a recency marker — either a legacy `<version>`-keyed dir
/// from before content addressing, or one a concurrent process is mid-creating.
/// Silent by design: a message here would depend on machine cache state and
/// break the stderr determinism the snapshot tests rely on.
fn evict_stale(cache_root: &Path, keep: &str, now: SystemTime, ttl: Duration) {
    let Ok(entries) = std::fs::read_dir(cache_root) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        if entry.file_name().to_str() == Some(keep) {
            continue;
        }
        let Ok(marker) = std::fs::metadata(entry.path().join(LAST_USED)) else {
            continue; // no marker → in-creation or legacy: leave it
        };
        let idle = marker
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok());
        if idle.is_some_and(|d| d > ttl) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_is_deterministic_and_content_sensitive() {
        let base: &[(&str, &[u8])] = &[("a/x.rs", b"hello"), ("b/y.rs", b"world")];
        let k = cache_key(base);
        assert_eq!(k.len(), 16, "16 hex chars");
        assert_eq!(k, cache_key(base), "deterministic");

        // A flipped content byte, a flipped path byte, and a reorder each move
        // the key — the disjointness that keeps heterogeneous binaries apart.
        assert_ne!(k, cache_key(&[("a/x.rs", b"hellO"), ("b/y.rs", b"world")]));
        assert_ne!(k, cache_key(&[("a/X.rs", b"hello"), ("b/y.rs", b"world")]));
        assert_ne!(k, cache_key(&[("b/y.rs", b"world"), ("a/x.rs", b"hello")]));
        // The 0x00 separators forbid a byte drifting across the path/content
        // boundary from aliasing.
        assert_ne!(
            cache_key(&[("a", b"bc")]),
            cache_key(&[("ab", b"c")]),
            "path/content boundary must not alias"
        );
    }

    #[test]
    fn evict_stale_reaps_only_long_idle_marked_siblings() {
        fn variant(root: &Path, name: &str, marker_age: Option<Duration>, now: SystemTime) {
            let dir = root.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("payload"), b"x").unwrap();
            if let Some(age) = marker_age {
                let marker = dir.join(LAST_USED);
                std::fs::write(&marker, b"").unwrap();
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(&marker)
                    .unwrap()
                    .set_modified(now - age)
                    .unwrap();
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let now = SystemTime::now();
        let ttl = Duration::from_secs(14 * 24 * 60 * 60);
        let day = Duration::from_secs(24 * 60 * 60);
        // `current` carries an ancient marker to prove the keep guard is by
        // name, not by age.
        variant(root, "current", Some(30 * day), now);
        variant(root, "idle", Some(30 * day), now);
        variant(root, "recent", Some(day), now);
        variant(root, "nomarker", None, now);

        evict_stale(root, "current", now, ttl);

        assert!(
            root.join("current").is_dir(),
            "the current dir is never reaped"
        );
        assert!(
            !root.join("idle").exists(),
            "a 30-day-idle sibling is reaped"
        );
        assert!(
            root.join("recent").is_dir(),
            "a 1-day-idle sibling survives"
        );
        assert!(
            root.join("nomarker").is_dir(),
            "a markerless (in-creation / legacy) dir is left alone"
        );
    }

    #[test]
    fn materialize_dir_is_hash_keyed_with_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let source = ExtractorSource::Vendored {
            cache_root: tmp.path().to_path_buf(),
        };
        let (pkg, _) = source.materialize().unwrap();
        let root = pkg.parent().unwrap();
        assert_eq!(
            root.parent().unwrap(),
            tmp.path(),
            "one hash dir under the root"
        );
        assert_eq!(
            root.file_name().unwrap().to_str().unwrap(),
            cache_key(VENDORED_FILES),
            "the cache dir is named by the source hash"
        );
        assert!(
            root.join(LAST_USED).is_file(),
            "materialize records a recency marker"
        );
    }

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
