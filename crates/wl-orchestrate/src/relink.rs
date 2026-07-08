//! Freshness routing for the extractor dylib — why the dylib reaches dylint
//! through an mtime-keyed hard link instead of its canonical path.
//!
//! The completeness guard's forced re-lint bumps the dylib's mtime, which
//! re-dirties member units through the **file dep** dylint's driver records
//! for the dylib. That channel has a hole: the driver inserts the file dep
//! only when `CARGO_PRIMARY_PACKAGE` is set (its `no_deps` gate), and cargo
//! sets that variable target-selection-aware — a `[lib] test = false` member
//! compiles under `--tests` as a plain dependency, without it. If such a unit
//! recompiles during a forced re-lint, its rewritten dep-info *loses* the
//! dylib entry, every future bump is invisible to it, and the guard
//! hard-errors "still missing after a forced re-lint" until a source edit or
//! a `target/dylint` wipe (verified end-to-end, 2026-07).
//!
//! The repair routes the bump through the driver's other channel: it records
//! the `DYLINT_LIBS` **value** as an env-dep for every unit it wraps,
//! unconditionally — including the non-primary recompiles that lose the file
//! dep. So the path handed to dylint encodes the dylib's own mtime
//! (`relint-<nanos>/<filename>`): bumping the mtime changes the next run's
//! `DYLINT_LIBS` value, which dirties exactly the workspace members' units
//! (registry deps never meet the wrapper, so warm-cache economics are
//! untouched). The mtime doubles as the persisted generation counter — no
//! extra state, and a steady-state dylib keeps a stable path.
//!
//! The per-generation path must be a **hard link**: dylint canonicalizes lib
//! paths, which resolves a symlink straight back to the canonical spelling —
//! a hard link is its own directory entry and survives (and `fs::hard_link`
//! works unprivileged on Windows, unlike symlinks). Links share the inode, so
//! a bump through either path moves the one true mtime.

use std::path::{Path, PathBuf};

/// How many generation directories to retain besides the current one. A
/// concurrent workspace-lint process (the dylib cache is shared per source
/// hash) may still be mid-run on the previous generation; pruning it would
/// fail that process's next dlopen. Surviving two bumps within one run is not
/// a realistic overlap.
const KEEP_PREVIOUS: usize = 1;

/// The extractor dylib as handed to `dylint::run`: canonical location plus
/// the mtime-keyed generation path derivation (module doc has the full why).
pub(super) struct RelinkedDylib {
    canonical: PathBuf,
}

impl RelinkedDylib {
    pub(super) fn new(canonical: PathBuf) -> Self {
        Self { canonical }
    }

    /// The canonical dylib location — the diagnostics/logging surface.
    pub(super) fn canonical(&self) -> &Path {
        &self.canonical
    }

    /// The path to hand to dylint for the dylib's current generation:
    /// `<parent>/relint-<mtime-nanos>/<filename>`, hard-linked on demand,
    /// stale generations pruned. Falls back to the canonical path (with a
    /// warning) if derivation fails — behavior then degrades to the
    /// file-dep-only channel, i.e. exactly the pre-relink mechanism.
    pub(super) fn current(&self) -> PathBuf {
        match self.link_generation() {
            Ok(path) => path,
            Err(e) => {
                eprintln!(
                    "wl-engine: cannot hard-link the extractor dylib for freshness routing \
                     ({e}) — falling back to {}",
                    self.canonical.display()
                );
                self.canonical.clone()
            }
        }
    }

    /// Start a new generation: bump the dylib's mtime so the next
    /// [`current`](Self::current) derives a different path (the env-dep
    /// channel) and the file-dep channel sees a newer file.
    ///
    /// The dylib is shared across concurrent workspace-lint processes, so the
    /// handle must not conflict with a simultaneous `LoadLibraryExW`/`dlopen`
    /// in another process's driver. On Windows a write-class handle
    /// (`append`) makes that load fail with a sharing violation;
    /// `FILE_WRITE_ATTRIBUTES`-only access is sufficient for `set_modified`
    /// and invisible to the loader. Unix has no such conflict.
    pub(super) fn bump(&self) -> std::io::Result<()> {
        #[cfg(windows)]
        let f = {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
            std::fs::OpenOptions::new()
                .access_mode(FILE_WRITE_ATTRIBUTES)
                .open(&self.canonical)?
        };
        #[cfg(not(windows))]
        let f = std::fs::OpenOptions::new()
            .append(true)
            .open(&self.canonical)?;
        f.set_modified(std::time::SystemTime::now())
    }

    fn link_generation(&self) -> std::io::Result<PathBuf> {
        let parent = self.canonical.parent().ok_or_else(|| {
            std::io::Error::other(format!("{} has no parent dir", self.canonical.display()))
        })?;
        let file_name = self.canonical.file_name().ok_or_else(|| {
            std::io::Error::other(format!("{} has no file name", self.canonical.display()))
        })?;
        let generation = generation_of(&self.canonical)?;
        let dir = parent.join(format!("relint-{generation}"));
        std::fs::create_dir_all(&dir)?;
        let link = dir.join(file_name);
        match std::fs::hard_link(&self.canonical, &link) {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            other => other?,
        }
        prune_generations(parent, generation);
        Ok(link)
    }
}

/// The dylib's mtime as nanoseconds since the epoch — the generation key.
fn generation_of(dylib: &Path) -> std::io::Result<u128> {
    let mtime = std::fs::metadata(dylib)?.modified()?;
    Ok(mtime
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos())
}

/// Best-effort removal of old `relint-<n>` sibling dirs: keep the current
/// generation plus the [`KEEP_PREVIOUS`] highest-numbered others (a stale
/// generation holds the dylib's old inode alive — real disk after a rebuild).
fn prune_generations(parent: &Path, current: u128) {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    let mut stale: Vec<(u128, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let n: u128 = name.to_str()?.strip_prefix("relint-")?.parse().ok()?;
            (n != current && entry.file_type().ok()?.is_dir()).then(|| (n, entry.path()))
        })
        .collect();
    stale.sort_by_key(|(n, _)| std::cmp::Reverse(*n));
    for (_, path) in stale.into_iter().skip(KEEP_PREVIOUS) {
        let _ = std::fs::remove_dir_all(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_dylib(dir: &Path) -> RelinkedDylib {
        let path = dir.join("libwl_extractor@nightly-test.dylib");
        std::fs::write(&path, b"dylib bytes").unwrap();
        RelinkedDylib::new(path)
    }

    #[test]
    fn current_is_a_generation_keyed_hard_link() {
        let tmp = tempfile::tempdir().unwrap();
        let dylib = fake_dylib(tmp.path());

        let link = dylib.current();
        assert_ne!(link, *dylib.canonical());
        assert_eq!(link.file_name(), dylib.canonical().file_name());
        let dir_name = link
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap();
        assert!(dir_name.starts_with("relint-"), "{dir_name}");
        assert_eq!(std::fs::read(&link).unwrap(), b"dylib bytes");
        // Stable across calls while the mtime is: the warm path.
        assert_eq!(dylib.current(), link);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(
                std::fs::metadata(&link).unwrap().ino(),
                std::fs::metadata(dylib.canonical()).unwrap().ino(),
                "must be a hard link, not a copy"
            );
        }
    }

    #[test]
    fn bump_rotates_the_generation_path() {
        let tmp = tempfile::tempdir().unwrap();
        let dylib = fake_dylib(tmp.path());
        // Anchor the first generation well in the past so the bump's `now`
        // is guaranteed to differ even on coarse-mtime filesystems.
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        std::fs::OpenOptions::new()
            .append(true)
            .open(dylib.canonical())
            .unwrap()
            .set_modified(old)
            .unwrap();

        let before = dylib.current();
        dylib.bump().unwrap();
        let after = dylib.current();
        assert_ne!(before, after, "a bump must change the DYLINT_LIBS value");
        // The previous generation survives one rotation (a concurrent run may
        // still be loading through it).
        assert!(before.exists());
    }

    #[test]
    fn prune_keeps_current_plus_one_previous() {
        let tmp = tempfile::tempdir().unwrap();
        let dylib = fake_dylib(tmp.path());
        for n in [10_u128, 20, 30] {
            std::fs::create_dir(tmp.path().join(format!("relint-{n}"))).unwrap();
        }

        let link = dylib.current();
        let survivors: Vec<String> = std::fs::read_dir(tmp.path())
            .unwrap()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_str()?.to_string();
                name.starts_with("relint-").then_some(name)
            })
            .collect();
        // Current generation + relint-30; relint-10 and relint-20 pruned.
        assert_eq!(survivors.len(), 2, "{survivors:?}");
        assert!(survivors.contains(&"relint-30".to_string()));
        let current_dir = link.parent().unwrap().file_name().unwrap();
        assert!(survivors.iter().any(|s| s.as_str() == current_dir));
    }
}
