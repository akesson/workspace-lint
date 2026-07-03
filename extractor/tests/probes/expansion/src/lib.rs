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

// CRLF-pinned file (see its `//!` doc and the sibling `.gitattributes`):
// on-disk byte-offset fidelity for spans in `\r\n` sources. Declared last so
// the line-number assertions above stay put.
mod crlf;

// Inherent-impl probes (PR 10): `ItemFact::self_type`, incl. the
// remote-module impl whose `def_path_str` rendering hides the type.
mod inherent;

// Use-site spans + glob discrimination (PR 11): the architecture lint anchors
// its diagnostics at the referencing line, and must tell `use m::*` (imports
// the module's *contents*) from `use a::m` (imports the module's *name*) —
// both resolve to the same module def in HIR; only `UseKind` differs.
pub mod globbed {
    pub fn glob_target() -> u32 {
        8
    }
}
mod glob_user {
    use super::globbed::*;

    pub fn calls_glob_target() -> u32 {
        glob_target() // probe: use-site-anchor
    }
}
mod named_user {
    use super::globbed;

    pub fn calls_named() -> u32 {
        globbed::glob_target()
    }
}
mod renamed_user {
    use super::globbed::glob_target as renamed_target;

    pub fn calls_renamed() -> u32 {
        renamed_target()
    }
}
