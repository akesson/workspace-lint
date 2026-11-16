//! Impl facts (schema 14): a type's OWN impls are part of the type.
//!
//! Each fragment lists its local impls of local ADTs
//! ([`wl_ir::IrFragment::impls`]) and each edge points at the outermost impl
//! it originates in ([`wl_ir::RefEdge::from_impl`]). Three derivations hang
//! off those ground facts:
//!
//! - **Own-impl edges** — a reference from inside an impl of its own target
//!   (`impl Foo { pub fn m(&self) }`'s `&self`, a derive's expansion) is
//!   neither a use-site nor signature exposure.
//! - **Removal reach** — deleting a type takes the edges of its impls with
//!   it, so its derives stop crediting imports and callees.
//! - **Written impls** — a type with a hand-written impl can't be deleted
//!   alone (the impl would be orphaned); derives go with the item.

use wl_ir::{ArchivedImplFact, ArchivedIrFragment, ArchivedRefEdge};

use super::assembly::Assembly;
use super::removal::RemovalSet;

impl Assembly {
    /// The impl an edge originates in — [`wl_ir::RefEdge::from_impl`]
    /// resolved through its fragment's `impls` table.
    fn origin_impl<'f>(
        frag: &'f ArchivedIrFragment,
        e: &ArchivedRefEdge,
    ) -> Option<&'f ArchivedImplFact> {
        let i = e.from_impl.as_ref()?.to_native();
        frag.impls.get(i as usize)
    }

    /// Is `e` a reference from inside its target's OWN impl (header, `&self`,
    /// a derive's expansion)? Part of the type, not a use of it: it neither
    /// counts as a use-site nor exposes the type in a public signature
    /// (narrowing the type still compiles).
    pub(super) fn is_own_impl_edge(frag: &ArchivedIrFragment, e: &ArchivedRefEdge) -> bool {
        Self::origin_impl(frag, e).is_some_and(|f| f.self_key.as_str() == e.to_key.as_str())
    }

    /// Does deleting `removed` take this edge with it? Its enclosing def is
    /// removed (or nested under one), or it originates in an impl of a
    /// removed type — a deleted type's derives go with it, and their
    /// derive-macro / body references must stop crediting imports and
    /// callees (a deleted `#[derive(Serialize)]` type left `use
    /// serde::Serialize` dangling otherwise).
    pub(super) fn edge_removed(
        &self,
        removed: &RemovalSet,
        frag: &ArchivedIrFragment,
        e: &ArchivedRefEdge,
    ) -> bool {
        removed.covers(&e.from)
            || Self::origin_impl(frag, e)
                .and_then(|f| self.defs.get(f.self_key.as_str()))
                .is_some_and(|d| removed.contains_id(&d.path))
    }

    /// Is `key` re-exported through a glob/module re-export? See
    /// [`Assembly::glob_reexported`].
    pub(crate) fn is_glob_reexported(&self, key: &str) -> bool {
        self.glob_reexported.contains(key)
    }
}
