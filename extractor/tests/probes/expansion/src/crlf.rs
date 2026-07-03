//! CRLF probe: this file is pinned to `\r\n` line endings via the sibling
//! `.gitattributes` (`-text`: no conversion on checkout or commit), so every
//! platform exercises the on-disk-offset mapping. rustc normalizes CRLF to
//! LF while loading a source file; a span emitted in those normalized
//! coordinates lands one byte early per preceding `\r` when sliced against
//! the raw file. The assertions on these items are byte-exact against the
//! raw bytes, guarding the `--fix` write surface for CRLF checkouts
//! (Windows `core.autocrlf=true` produces exactly this layout).

/// Hand-written `pub` behind eight CRLF lines: a normalized-coordinate
/// emission would slice eight bytes early.
pub fn crlf_probed() -> u32 {
    100
}

/// Restricted form, further down still.
pub(crate) fn crlf_crate_only() -> u32 {
    200
}
