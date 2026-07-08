//! P6b: a CROSS-crate exported macro's `$crate::…` expansion (the
//! `tracing::debug!` shape). The local-macro case (`widget!`) reaches HIR
//! with a literal `$crate` segment; this pins what the cross-crate case
//! looks like — the glob accounting's name-evidence blinding depends on it.
pub fn logs() {
    macrodep::ext_event!();
}
