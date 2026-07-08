//! Call-graph accessors over the assembled model: which fn encloses a source
//! byte offset, what a fn calls, and who calls it.
//!
//! Built lazily — a single walk of every config's edges — behind a `OnceLock`
//! on [`super::SemanticModel`], so nothing here runs unless a lint asks and the
//! assemble hot path is untouched. The refactoring classifier behind
//! `duplicate-code` is the sole consumer: it resolves a clone instance's source
//! byte span to its enclosing fn identity ([`CallGraph::enclosing_fn`]), then
//! compares the instances' callee sets ([`CallGraph::callees_of`] — the
//! IR-confirm contract: two "identical" fns that resolve different callees are
//! not interchangeable) and partitions their inbound references
//! ([`CallGraph::references_to`] — merge-and-redirect vs delete-the-dead-copy).
//!
//! Identity is the same `::`-joined display path the cfg-matrix union keys on
//! ([`super::DefInfo::path`]): for a local item its `RefEdge::from` renders to
//! exactly that path, so out-edges bucket by from-identity with no key lookup,
//! and [`Assembly::target_identity`](super::assembly) resolves a target to it
//! through the full cross-config join.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use wl_ir::ArchivedRefEdge;

use super::assembly::{Assembly, FastMap};

/// The innermost `fn` enclosing a byte offset — see
/// [`SemanticModel::enclosing_fn`](super::SemanticModel::enclosing_fn).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnclosingFn {
    /// The fn's cross-config stable identity (its `::`-joined definition path)
    /// — the handle [`callees_of`](CallGraph::callees_of) /
    /// [`references_to`](CallGraph::references_to) key on.
    pub identity: String,
    pub krate: String,
    /// The whole-item span (attrs/doc through the closing brace).
    pub full_span: wl_ir::Span,
}

/// One resolved call target of a fn — see
/// [`SemanticModel::callees_of`](super::SemanticModel::callees_of).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Callee {
    /// For a workspace target: its stable identity (through the join). For an
    /// out-of-workspace target (std / third-party): the display path as
    /// written. The two never collide — an unresolved display path is not a
    /// workspace identity — so callee-set equality is a faithful "calls the
    /// same things" test even when the difference is which `std` fn is called.
    pub target: String,
    pub kind: String,
    /// `true` iff `target` is a workspace identity (resolved through the join),
    /// `false` for an out-of-workspace display path.
    pub resolved: bool,
}

/// One inbound reference to a def — see
/// [`SemanticModel::references_to`](super::SemanticModel::references_to).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct InboundRef {
    /// The referring item's identity (`::`-joined). For a call site this is the
    /// enclosing item; the classifier drops referrers that are themselves group
    /// instances (recursion / mutual calls among the copies).
    pub from: String,
    /// The use-site span (`None` for lowered-signature edges and dummy spans).
    pub span: Option<wl_ir::Span>,
    /// This edge sits inside a `use` declaration (an import / re-export), not a
    /// value use-site — call-site counting filters these out, delete-dead-copy
    /// keeps them (a `use` of the copy still names it).
    pub import: bool,
    /// This edge came from the signature-position walk, not a body use-site.
    pub in_signature: bool,
}

/// A 12-byte locator into `configs[config].fragments[frag].references[edge]`.
/// The index stores these rather than materialized records, so the whole graph
/// costs ~three `u32`s per edge and a record is read from the mmap'd archive
/// only for the handful of identities a query touches.
#[derive(Clone, Copy)]
struct EdgeLoc {
    config: u16,
    frag: u32,
    edge: u32,
}

/// A fn's whole-item span + identity, bucketed by file for the containment scan.
struct FnSpan {
    span: wl_ir::Span,
    identity: String,
    krate: String,
}

/// The lazily-built call-graph indexes (see the module docs).
pub(super) struct CallGraph {
    /// from-identity (`::`-joined [`ArchivedRefEdge`]`::from`) → its edges. A
    /// `BTreeMap` so nested-def aggregation (a fn's own edges plus those of
    /// closures / fns defined under it) is one contiguous prefix range.
    out_edges: BTreeMap<String, Vec<EdgeLoc>>,
    /// resolved-target-identity → the edges landing on it. Only workspace
    /// identities are keyed (an out-of-workspace target is never a query
    /// subject).
    in_edges: BTreeMap<String, Vec<EdgeLoc>>,
    /// file → the fns defined in it, for `enclosing_fn`'s containment scan.
    fns_by_file: FastMap<PathBuf, Vec<FnSpan>>,
}

impl CallGraph {
    pub(super) fn build(configs: &[(String, Assembly)]) -> Self {
        let mut out_edges: BTreeMap<String, Vec<EdgeLoc>> = BTreeMap::new();
        let mut in_edges: BTreeMap<String, Vec<EdgeLoc>> = BTreeMap::new();
        let mut fns_by_file: FastMap<PathBuf, Vec<FnSpan>> = FastMap::default();
        // A fn extracted under several configs has the same on-disk span; the
        // lowest config index (primary first) wins its (file, identity) slot.
        let mut seen_fn: BTreeSet<(PathBuf, String)> = BTreeSet::new();

        for (ci, (_, asm)) in configs.iter().enumerate() {
            for def in asm.defs.values() {
                if def.kind != "fn" {
                    continue;
                }
                let Some(fs) = def.full_span.as_ref() else {
                    continue;
                };
                if fs.from_expansion {
                    continue;
                }
                let file = PathBuf::from(&fs.file);
                if !seen_fn.insert((file.clone(), def.path.clone())) {
                    continue;
                }
                fns_by_file.entry(file).or_default().push(FnSpan {
                    span: fs.clone(),
                    identity: def.path.clone(),
                    krate: def.krate.clone(),
                });
            }
            for (fi, frag) in asm.archived_fragments().enumerate() {
                for (ei, e) in frag.references.iter().enumerate() {
                    let loc = EdgeLoc {
                        config: ci as u16,
                        frag: fi as u32,
                        edge: ei as u32,
                    };
                    out_edges
                        .entry(wl_ir::join_paths(&e.from, "::"))
                        .or_default()
                        .push(loc);
                    if let Some(id) = asm.target_identity(e) {
                        in_edges.entry(id.to_owned()).or_default().push(loc);
                    }
                }
            }
        }

        CallGraph {
            out_edges,
            in_edges,
            fns_by_file,
        }
    }

    /// Deref a locator back to its (assembly, edge) — an O(1) archive cast.
    fn edge_at(configs: &[(String, Assembly)], loc: EdgeLoc) -> (&Assembly, &ArchivedRefEdge) {
        let asm = &configs[loc.config as usize].1;
        let e = &asm.archived_fragment(loc.frag as usize).references[loc.edge as usize];
        (asm, e)
    }

    /// The innermost `fn` whose whole-item span contains `offset` in `file`
    /// (`lo <= offset < hi`; smallest interval wins, so a nested fn beats its
    /// enclosing one). `None` if no fn contains the offset — a synthetic or
    /// macro-generated body (skipped at build), or an offset outside any fn.
    pub(super) fn enclosing_fn(&self, file: &Path, offset: u32) -> Option<EnclosingFn> {
        self.fns_by_file
            .get(file)?
            .iter()
            .filter(|f| f.span.lo <= offset && offset < f.span.hi)
            .min_by_key(|f| f.span.hi - f.span.lo)
            .map(|f| EnclosingFn {
                identity: f.identity.clone(),
                krate: f.krate.clone(),
                full_span: f.span.clone(),
            })
    }

    /// Every non-import call target of the fn `identity` and of the defs nested
    /// under it, unioned across configs. Import / trait-scope edges are excluded
    /// (a `use` in scope is not a call). A `BTreeSet` so the result is
    /// order-independent — its equality with another instance's set is the
    /// IR-confirm test.
    pub(super) fn callees_of(
        &self,
        configs: &[(String, Assembly)],
        identity: &str,
    ) -> BTreeSet<Callee> {
        let mut out = BTreeSet::new();
        for loc in self.covered_out(identity) {
            let (asm, e) = Self::edge_at(configs, loc);
            if e.import || e.trait_scope {
                continue;
            }
            let (target, resolved) = match asm.target_identity(e) {
                Some(id) => (id.to_owned(), true),
                None => (wl_ir::join_paths(&e.to, "::"), false),
            };
            out.insert(Callee {
                target,
                kind: e.to_kind.as_str().to_owned(),
                resolved,
            });
        }
        out
    }

    /// Every inbound reference to `identity`, deduped across cfg variants (a
    /// plain and a `+test` unit emit the same edge under different key
    /// generations, same identity + span) and sorted. Flags are surfaced, not
    /// pre-filtered: the caller counts call sites (`!import && !in_signature`)
    /// or keeps every referrer (dead-copy detection) as it needs.
    pub(super) fn references_to(
        &self,
        configs: &[(String, Assembly)],
        identity: &str,
    ) -> Vec<InboundRef> {
        let Some(locs) = self.in_edges.get(identity) else {
            return Vec::new();
        };
        let mut seen: BTreeSet<InboundRef> = BTreeSet::new();
        for &loc in locs {
            let (_asm, e) = Self::edge_at(configs, loc);
            seen.insert(InboundRef {
                from: wl_ir::join_paths(&e.from, "::"),
                span: e.span.as_ref().map(wl_ir::Span::from),
                import: e.import,
                in_signature: e.in_signature,
            });
        }
        seen.into_iter().collect()
    }

    /// A fn's own out-edges plus those of every def nested under it: the exact
    /// `identity` bucket, then the contiguous `identity::` prefix block. Keying
    /// the prefix on a full `::` segment boundary is what keeps `foo::bar`
    /// (nested) in and `foo_bar` (a sibling) out — the range starts at
    /// `identity::`, which sorts strictly after any `identity`-with-a-word-char
    /// sibling, so no non-nested key slips in.
    fn covered_out(&self, identity: &str) -> Vec<EdgeLoc> {
        let mut locs: Vec<EdgeLoc> = Vec::new();
        if let Some(v) = self.out_edges.get(identity) {
            locs.extend(v.iter().copied());
        }
        let prefix = format!("{identity}::");
        for (k, v) in self.out_edges.range(prefix.clone()..) {
            if !k.starts_with(&prefix) {
                break;
            }
            locs.extend(v.iter().copied());
        }
        locs
    }
}
