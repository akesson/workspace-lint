//! The minimal cross-phase IR for the step-0 spike.
//!
//! This is the serialization contract between Phase 1 (the per-crate driver,
//! which has `TyCtxt`) and Phase 2 (the assembler, which does not). Per
//! `SPIKE-rustc-fidelity-tree.md` §6 this schema is *both* the internal IR and
//! the public extension surface, so it is intentionally plain data. It carries
//! what's needed to diff definitions against the syn resolver (path, kind,
//! visibility, byte-span) plus the **reference graph** (who-uses-whom) that will
//! back the usage lints. Macros/cfg come later.

use serde::{Deserialize, Serialize};

/// One crate's contribution to the IR, emitted during that crate's compilation
/// and written to `$WL_IR_OUT/<crate>.json`. Phase 2 assembles these.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IrFragment {
    /// Code-form crate name (hyphens → underscores) — the leading segment of
    /// every canonical path, matching the syn resolver's `ResolvedPath[0]`.
    pub crate_name: String,
    pub items: Vec<ItemFact>,
    /// Resolved reference edges harvested from this crate's HIR: a `from` local
    /// item *uses* a `to` def (local or cross-crate). This is the reference graph
    /// that will back the usage lints (`unused-pub`, `unused-deps`, architecture
    /// rules) — rustc's resolved answer to syn's text-based occurrence model.
    /// Deduped and sorted so the fragment is deterministic (byte-identical across
    /// driver/dylib). `#[serde(default)]` keeps pre-references fragments loadable.
    #[serde(default)]
    pub references: Vec<RefEdge>,
}

/// One resolved reference: local item `from` mentions def `to`. Both carry a
/// human `path` (`[crate_name, ..]`) *and* a cross-crate-stable `key`.
///
/// **The `path` is display-only; joins go through the `key`.** `def_path_str`
/// (which produces `path`) is not a stable cross-crate identity: a defining
/// crate renders a def at its *definition* path (`syn_workspace::resolve::
/// workspace::Workspace`) while a downstream crate renders the *same* def at its
/// re-export path (`syn_workspace::Workspace`, via rustc's visible-parent map).
/// So a `to.path` from crate A never string-matches the `ItemFact.path` in the
/// crate that defines it — the whole per-crate `path`-equality join fails 0/215
/// cross-crate on this workspace. `to_key` / `from_key` are the `DefPathHash`
/// (hash of `StableCrateId` + local def path), identical no matter which crate
/// observes the def — the stable key SPIKE §5.4 calls for. Reverse indexes join
/// `to_key` against [`ItemFact::key`].
///
/// Deduped per fragment, so a `from` that calls `to` five times yields one edge.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RefEdge {
    pub from: Vec<String>,
    pub to: Vec<String>,
    /// Stable `DefPathHash` (hex) of the enclosing `from` item — always local.
    #[serde(default)]
    pub from_key: String,
    /// Stable `DefPathHash` (hex) of the referenced `to` def. Joins to
    /// [`ItemFact::key`] regardless of which crate the reference came from.
    #[serde(default)]
    pub to_key: String,
    /// The referenced def's `DefKind` in the shared vocabulary (best-effort).
    pub to_kind: String,
    /// `true` iff `to` lives in another crate (`to[0] != crate_name`).
    pub external: bool,
    /// `true` iff this edge is the path *inside* a `use` / `pub use` declaration
    /// (the enclosing `from` is a `DefKind::Use`) — an import/re-export, not a
    /// value/type use-site. Importing or re-exporting a name does **not** make it
    /// non-dead (rustc's own dead-code analysis treats `use` specially), so the
    /// unused-pub reverse index must count only `!import` edges. Every *real* use
    /// of the name emits its own `import: false` edge, so discounting these is
    /// safe. Kept (not dropped) because other consumers — `unused-deps`, "what
    /// does this module re-export" — legitimately want import edges.
    #[serde(default)]
    pub import: bool,
}

/// A single resolved definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemFact {
    /// Canonical path segments: `[crate_name, module.., name]`. Display-only —
    /// this is `def_path_str`, which is *not* stable cross-crate (see
    /// [`RefEdge`]); use [`ItemFact::key`] to join references to this def.
    pub path: Vec<String>,
    /// Cross-crate-stable `DefPathHash` (hex) of this def — the join key a
    /// [`RefEdge::to_key`] matches against to build the workspace reverse index.
    #[serde(default)]
    pub key: String,
    /// rustc `DefKind`, stringified into the syn model's vocabulary
    /// (`struct`/`enum`/`fn`/`trait`/…). Note this deliberately collapses
    /// `AssocFn→fn` / `AssocConst→const`, so it alone can't tell a free def from
    /// an associated one — that's what [`ItemFact::parent_kind`] is for.
    pub kind: String,
    /// The **parent's** `DefKind`, in a small closed vocabulary
    /// (`mod`/`impl`/`trait`/`fn`/`const`/`static`/`closure`/`other`), or `None`
    /// for the crate root. This is the principled container signal: `mod` ⇒ a
    /// module-level (syn-representable) def; `impl`/`trait` ⇒ an associated item;
    /// anything else ⇒ body-nested (fn-local). Replaces the downstream
    /// snake_case-path heuristic the fidelity oracle used to guess this.
    pub parent_kind: Option<String>,
    /// For a **trait-impl** associated item, the stable `key` of the trait item
    /// it implements (`<T as Tr>::f` ⇒ `Tr::f`); `None` for inherent-impl items,
    /// trait *declaration* items, module-level and fn-local defs. Combined with
    /// `parent_kind`, this is what tells an **inherent** impl method (a real
    /// unused-pub candidate — independently-controllable pub API, judged by its
    /// direct-call edges) from a **trait-impl** method (reachable via trait
    /// dispatch the ref graph doesn't edge, visibility not independent — excluded,
    /// as rustc's own dead_code does). Also the trait→impls linkage for
    /// dispatch-reachability.
    #[serde(default)]
    pub trait_item: Option<String>,
    pub visibility: Visibility,
    /// Byte span in the original source, or `None` for synthetic defs.
    pub span: Option<Span>,
}

/// Normalized visibility. `Restricted` carries the rendered restriction
/// (`"crate"`, or a module path) so the diff against syn's
/// `Public`/`PubCrate`/`PubSuper`/`PubIn`/`Private` can normalize later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Visibility {
    Public,
    Restricted(String),
}

/// A byte range within a workspace-relative source file — the `--fix` write
/// surface. Byte offsets are relative to the start of `file` (not the global
/// `SourceMap` coordinate).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub file: String,
    pub lo: u32,
    pub hi: u32,
}
