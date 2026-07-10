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

// Signature exposure, predicate/bounds family (check 23): defs named only in
// bounds / where-clauses / supertraits / item bounds / field types.
pub mod sig_exposure;

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

// Written-root recording (`RefEdge::via`): a dep whose surface is another
// crate's items re-exported (`shim` re-exports `std::time::Duration`). rustc
// resolves the whole path into core/std, so without `via` the shim dep is
// invisible to unused-deps — the `web-time` FP class.
mod via_user {
    // Resolves through the shim into core/std → via = Some("shim").
    use shim::Duration;
    // Defined by the shim itself → written root == defining crate → via = None.
    use shim::OwnItem;

    pub fn wait(_how_long: Duration) -> OwnItem {
        // Fully-qualified code path through the shim — the non-`use` shape.
        let _zero = shim::Duration::from_secs(0);
        OwnItem
    }
}

/// A documented, attributed fn — the delete surface must swallow the doc
/// comment AND the outer attributes, or a `--fix` delete orphans them
/// (E0585 / E0658).
#[inline]
#[allow(clippy::let_and_return)]
pub fn attributed() -> u32 {
    5
}

// Brace-list surgery surface (unused-pub `--fix` deletes dangling `use`
// leaves). Two leaves lowered from one declaration must share a `decl_span`
// (the whole `use …;`) yet carry distinct `elem_span`s (the leaf as written,
// physically inside the braces). A brace-list alias leaf must span
// `aliased_src as al`.
pub mod listed {
    pub fn first() -> u32 {
        1
    }
    pub fn second() -> u32 {
        2
    }
    pub fn aliased_src() -> u32 {
        3
    }
}
mod list_user {
    use super::listed::{first, second};
    use super::listed::{aliased_src as al};

    pub fn uses() -> u32 {
        first() + second() + al()
    }
}

// Nested-path-inside-a-brace (`use a::{b::c, d}`) — the form a naive
// last-segment `elem_span` would delete wrongly (leaving `b::`). The leaf's
// written span must cover the whole brace entry `deep::buried`.
pub mod nested {
    pub mod deep {
        pub fn buried() -> u32 {
            7
        }
    }
    pub fn shallow() -> u32 {
        8
    }
}
mod nested_user {
    use super::nested::{deep::buried, shallow};

    pub fn uses() -> u32 {
        buried() + shallow()
    }
}

// --- `loaded_files` probes -------------------------------------------------
// Each construct below names a file that a *syntactic* walker cannot resolve.
// rustc opens them all, which is what `IrFragment::loaded_files` records and
// what the `orphan-file` lint judges. See probe.rs check 21.

// Multi-arm `cfg_attr` path: exactly one arm compiles. Keyed on `test` rather
// than `unix`/`windows` so the expected set is identical on all three CI OSes.
#[cfg_attr(test, path = "imp_test.rs")]
#[cfg_attr(not(test), path = "imp_main.rs")]
mod imp;

// `include!` in expression position — invisible to an item walk.
pub static PROBE_TABLE: [u8; 4] = include!("gen_table.rs");

// `include_str!` of a `.rs` file: read as bytes, never parsed as source, yet
// rustc still registers it in the SourceMap so diagnostics can point into it.
pub const PROBE_SNIPPET: &str = include_str!("gen_snippet.rs");

pub fn probe_loaded_files() -> u32 {
    imp::val() + u32::from(PROBE_TABLE[0]) + PROBE_SNIPPET.len() as u32
}

// Token-passthrough bang macro from an external crate (the cfg_if shape):
// the macro re-emits OUR item tokens, so `from_passthrough_macro`'s span has
// root syntax context — no ExpnData chain to walk, no written path node. The
// reference graph records NOTHING for `passthrough`; only the resolver-level
// `used_crates` (schema 12) still names it. See probe.rs check 24.
passthrough::passthrough! {
    /// Documented so the `WL_UNDOCUMENTED_PUB` findings demo stays quiet here.
    pub fn from_passthrough_macro() -> u32 {
        11
    }
}
