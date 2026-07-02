//! Probe crate for the WS1 span-fidelity policy (SPIKE §12.7). Every item here
//! is a named assertion target in `spike/probe-check`. The point is the
//! macro/derive cases: their tokens live somewhere the user can't edit, so they
//! must carry no `vis_span` (and, for the cross-file macro, a whole-item `span`
//! that maps to the *invocation* site, not the macro definition).

#[macro_use]
mod gen;

// Cross-file macro: the `pub` token is in `gen.rs`. Generated fn must map to
// THIS line and carry no editable vis_span.
make_pub_fn!(from_cross_file_macro);

// Same-file macro: still an expansion, still no editable vis_span.
macro_rules! local_macro {
    ($name:ident) => {
        pub fn $name() -> u32 {
            7
        }
    };
}
local_macro!(from_local_macro);

// Derive-generated trait impls (`Clone`, `Debug`): their assoc fns are
// expansion-derived → span `from_expansion`, no vis_span.
#[derive(Clone, Debug)]
pub struct Probed {
    pub field: u32,
}

/// Documented so the `WL_UNDOCUMENTED_PUB` findings demo stays quiet here.
/// Plain hand-written `pub`: byte-exact vis_span expected.
pub fn plain() -> u32 {
    1
}

/// Restricted visibility: rustc captures the `pub(crate)` token; syn can't.
pub(crate) fn crate_only() -> u32 {
    2
}

// Private (inherited visibility): rustc lowers this to an empty span → no
// vis_span.
fn private() -> u32 {
    3
}

// Undocumented, public, uncalled — the A4 suggestion round-trip target.
pub fn undocumented_roundtrip() -> u32 {
    4
}

// FFI export (PR 9): carries `#[no_mangle]` — no Rust referrer will ever
// exist, so `ItemFact::attrs` is the only evidence it isn't dead pub API.
#[unsafe(no_mangle)]
pub extern "C" fn ffi_export() -> u32 {
    5
}

// Signature exposure (PR 9): `Probed` is named in this pub fn's signature, so
// the lowered-signature pass must emit an `in_signature` edge to it.
pub fn exposes_probed() -> Probed {
    Probed { field: 6 }
}
