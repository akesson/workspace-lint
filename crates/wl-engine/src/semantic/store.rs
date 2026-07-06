//! The zero-copy fragment backing store.
//!
//! An [`Assembly`](super::assembly::Assembly) owns its fragments for the whole
//! model lifetime (four query paths re-scan them). Instead of parsing each
//! fragment into an owned [`wl_ir::IrFragment`] tree — the JSON era's ~3 GB-RSS
//! cost on large workspaces — it keeps the raw bytes and reads the **archived**
//! view in place:
//!
//! - **Production** ([`FragmentBytes::open`]): mmap the on-disk `.wlir` file
//!   (16-byte header + rkyv archive), validate the header once, then serve
//!   `&ArchivedIrFragment` by an O(1) pointer cast on each access. The bytes
//!   stay file-backed (pageable), so the fragment set never counts as resident
//!   heap.
//! - **Fixtures/tests** ([`FragmentBytes::owned`]): the golden-fixture path
//!   builds native `IrFragment`s; it serializes each to a bare archive so the
//!   assembler sees the *same* archived representation as production.
//!
//! [`archived`](FragmentBytes::archived) recomputes the borrow from the owned
//! bytes on every call, so there is no self-referential struct: `Assembly` owns
//! the `Vec<FragmentBytes>` and every query borrows it for the call's duration.

use std::path::Path;

use wl_ir::ArchivedIrFragment;

/// One fragment's bytes, in whichever representation the source produced.
pub(super) enum FragmentBytes {
    /// Production: the whole on-disk file (header + archive), mmap'd.
    Mmap(memmap2::Mmap),
    /// Fixtures: a bare rkyv archive (no header), owned and 16-aligned. Only the
    /// golden-fixture tests build fragments in memory, so this arm is test-only.
    #[cfg(test)]
    Owned(rkyv::util::AlignedVec),
}

impl FragmentBytes {
    /// Open, mmap, and header-validate an on-disk `.wlir` fragment. The header
    /// gate (magic + current `SCHEMA_VERSION` + length) rejects stale/foreign/
    /// truncated files loudly — the completeness guard turns that into a
    /// re-extract. `WL_IR_CHECKED` additionally runs rkyv's full validation.
    pub(super) fn open(path: &Path) -> Result<Self, String> {
        let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        // SAFETY: the IR dir is workspace-lint's own cache. Fragments are
        // written atomically (temp file + rename) and never mutated in place,
        // so the mapping's bytes are stable for its whole lifetime.
        let mmap = unsafe { memmap2::Mmap::map(&file) }.map_err(|e| e.to_string())?;
        wl_ir::validate_header(&mmap).map_err(|e| e.to_string())?;
        if std::env::var_os("WL_IR_CHECKED").is_some() {
            wl_ir::access_archive_checked(&mmap).map_err(|e| e.to_string())?;
        }
        Ok(Self::Mmap(mmap))
    }

    /// Serialize a native fragment to its bare archive — the fixture seam that
    /// lets the golden-fixture tests feed the archived path from struct literals.
    /// Infallible in practice: our IR types have no fallible `Serialize`, so a
    /// failure here is a bug, not a runtime condition (fixture-only path).
    #[cfg(test)]
    pub(super) fn owned(fragment: &wl_ir::IrFragment) -> Self {
        Self::Owned(wl_ir::to_archive(fragment).expect("valid IR fragment serializes"))
    }

    /// The archived fragment, borrowed from the owned bytes. O(1) pointer cast;
    /// safe to call repeatedly (the four re-scan query paths do).
    #[inline]
    pub(super) fn archived(&self) -> &ArchivedIrFragment {
        match self {
            // SAFETY: header validated in `open`; buffer is our own atomic write.
            Self::Mmap(m) => unsafe { wl_ir::access_archive(m) },
            // SAFETY: produced by `to_archive` above (our own serialize) and
            // 16-aligned (AlignedVec), so the root cast is sound.
            #[cfg(test)]
            Self::Owned(v) => unsafe { rkyv::access_unchecked::<ArchivedIrFragment>(&v[..]) },
        }
    }
}
