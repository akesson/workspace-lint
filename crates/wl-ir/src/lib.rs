//! The cross-phase IR contract of workspace-lint's rustc-fidelity engine.
//!
//! This is the serialization contract between Phase 1 (the per-crate extractor
//! dylib, which has `TyCtxt`) and Phase 2 (the assembler, which does not). Per
//! `SPIKE-rustc-fidelity-tree.md` §6 this schema is *both* the internal IR and
//! the public extension surface, so it is intentionally plain data. It carries
//! resolved definitions (path, kind, visibility, byte-span) plus the
//! **reference graph** (who-uses-whom) that backs the usage lints.
//!
//! Compatibility model: *additive* growth uses `#[serde(default)]` fields (old
//! fragments stay loadable); a change that would make old fragments
//! **misleading** — a field's meaning shifts, an emit rule changes what a value
//! covers — bumps [`SCHEMA_VERSION`] instead, and loaders reject the mismatch
//! loudly rather than silently assembling skewed data.

use serde::{Deserialize, Serialize};

/// The schema version the extractor stamps into every [`IrFragment`] and
/// loaders assert with [`IrFragment::check_schema`]. The extractor and the
/// assembler ship in lockstep (the binary vendors the extractor source), so a
/// mismatch always means a stale cache or a hand-mixed fragment dir — never a
/// supported configuration. Pre-versioning fragments deserialize as `0` and
/// are rejected the same way.
pub const SCHEMA_VERSION: u32 = 1;

/// One crate's contribution to the IR, emitted during that crate's compilation
/// and written to `$WL_IR_OUT/<crate>.json`. Phase 2 assembles these.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IrFragment {
    /// The [`SCHEMA_VERSION`] of the extractor that wrote this fragment.
    /// `#[serde(default)]` so pre-versioning fragments deserialize (as `0`)
    /// and get rejected by [`IrFragment::check_schema`] with a real message
    /// instead of a serde error.
    #[serde(default)]
    pub schema_version: u32,
    /// Code-form crate name (hyphens → underscores) — the leading segment of
    /// every canonical path, matching the syn resolver's `ResolvedPath[0]`.
    pub crate_name: String,
    /// Which cargo target this fragment was extracted from: `"lib"`, `"bin"`,
    /// `"proc-macro"`, or `"test"` (integration test / bench). Cargo allows a
    /// package's bin target to share the lib's crate name (`src/lib.rs` +
    /// `src/main.rs`), so `crate_name` alone cannot key a fragment — without
    /// this the bin fragment used to *clobber* the lib's on disk and lib-only
    /// deps read as unused. Also the primary-units signal for the
    /// `architecture` lint (test targets legitimately reach across layers).
    /// `""` in pre-field fragments (unobservable in practice — the extractor
    /// ships vendored in lockstep).
    #[serde(default)]
    pub target_kind: String,
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

impl IrFragment {
    /// Rejects a fragment written under a different [`SCHEMA_VERSION`].
    /// Call this on every loaded fragment before assembling — skew detection
    /// is the loader's job, and silent acceptance of a stale fragment would
    /// assemble a tree that mixes two schema generations.
    pub fn check_schema(&self) -> Result<(), String> {
        if self.schema_version == SCHEMA_VERSION {
            return Ok(());
        }
        Err(format!(
            "IR fragment for `{}` has schema version {} but this build expects {}; \
             the fragment dir is stale or was written by a different extractor build \
             — delete it and re-extract",
            self.crate_name, self.schema_version, SCHEMA_VERSION
        ))
    }
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
    /// `true` iff this edge came from the *signature-position* walk
    /// (`fn_sig`/`type_of` type projections) rather than a body/path use-site.
    /// Backs `exposed_in_public_signature`: tightening an item that appears in
    /// a pub item's signature would break compilation (E0446 /
    /// `private_interfaces`), so the unused-pub `--fix` must not propose it.
    #[serde(default)]
    pub in_signature: bool,
    /// For an `import` edge: `true` iff the `use` declaration is itself `pub`
    /// — a re-export. Only these need the must-stay-`pub` guard (tightening
    /// the target breaks the `pub use`: E0364/E0365) and only these make the
    /// target externally reachable through the importing module. A plain
    /// same-crate `use` is neither; treating it as a re-export would let any
    /// test-mod `use super::*` shield a crate's whole root from the verdict.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reexport: bool,
    /// For an `import` edge: `true` iff the declaration is a glob
    /// (`use m::*`). The edge's `to` is the module both ways — this flag is
    /// what distinguishes importing the module's *name* (`use a::m`) from
    /// importing its *contents*, which the `architecture` lint judges
    /// differently (a glob is tested as a representative child of the target,
    /// so `deny = ["m::**"]` catches it).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub glob: bool,
    /// For a single-name `import` edge: the local binding name — differs from
    /// the target's own name under `use a::B as C` (the architecture lint's
    /// "imported locally as" rename note needs it; nothing else in the IR
    /// records the alias). `None` for glob/list-stem imports and non-imports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    /// The use-site: where in `from`'s source this reference occurs. `None`
    /// for lowered-signature-pass edges (no single HIR token) and dummy
    /// spans. For a macro-generated reference this is the *invocation site*
    /// (`Span::from_expansion` set). Kept **last** so the derived `Ord`
    /// compares edge identity first; the extractor dedups on identity alone
    /// and keeps the first (lowest) span — five calls to the same def are
    /// still one edge, anchored at the earliest use-site.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<Span>,
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
    /// For an **inherent-impl** associated item, the stable `key` of the
    /// impl's nominal self type — the external-reachability handle: the item
    /// is nameable from outside exactly when its self type is (`Type::method`
    /// hops through `Type`, wherever the `impl` block lives; `def_path_str`
    /// renders a remote impl at the *impl's* module, so a path-prefix lookup
    /// cannot recover the type). `None` for trait-impl items (dispatch-judged
    /// via [`Self::trait_item`]), non-assoc defs, and exotic self types
    /// (`&T`, tuples, primitives — no single nominal def to point at; treated
    /// as not externally reachable, the flag-more direction).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_type: Option<String>,
    pub visibility: Visibility,
    /// Byte span of the whole definition in the original source, or `None` for
    /// synthetic defs (the `--test` harness `main`, etc.). For a macro-generated
    /// item this is the *invocation site* (`Span::from_expansion` set) — good for
    /// "generated here" display, **not** an editable surface. See [`Span`].
    pub span: Option<Span>,
    /// Byte range of the visibility token (`pub` / `pub(crate)` / `pub(in path)`)
    /// — the `--fix` *tighten* write surface, mirroring syn's `vis_byte_range`
    /// but also present for the restricted forms syn can't capture. `None` when
    /// there is no editable token: private items (rustc lowers inherited
    /// visibility to an empty span at the item's first token), trait-declaration
    /// and trait-impl items (visibility isn't independently controllable — no
    /// token), the crate root, and any macro-generated item (the token lives in
    /// the macro definition, never a write surface). `#[serde(default)]` keeps
    /// pre-`vis_span` fragments loadable.
    #[serde(default)]
    pub vis_span: Option<Span>,
    /// The export-shaped attributes on this def, from the small closed set the
    /// reachability analysis roots on: `no_mangle`, `export_name`, `used`. An
    /// FFI-exported item has no Rust referrer, so without these it reads as
    /// dead pub API (the `ffi_no_mangle_export` known false positive this
    /// field exists to fix). Emitted names only — values (`export_name = "…"`)
    /// don't affect reachability.
    #[serde(default)]
    pub attrs: Vec<String>,
}

/// Normalized visibility. `Restricted` carries the rendered restriction
/// (`"crate"`, or a module path) so the diff against syn's
/// `Public`/`PubCrate`/`PubSuper`/`PubIn`/`Private` can normalize later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Visibility {
    Public,
    Restricted(String),
}

/// A byte range within a workspace-relative source file. Byte offsets are
/// **on-disk** positions relative to the start of `file` — NOT rustc's
/// internal coordinates, which live in a CRLF→LF/BOM-normalized copy of the
/// source: the extractor maps positions back through the file's
/// normalization records, so `raw_bytes[lo..hi]` is the span's text even in
/// a CRLF file (where every preceding `\r` shifts the raw position by one).
/// That property is what makes `lo..hi` a valid `--fix` write surface.
/// `line` is the 1-based line of `lo` (identical in both coordinate
/// systems) — emitted by the extractor (it has the `SourceMap`; re-deriving
/// lines downstream would duplicate file I/O and can drift from the
/// compiler's own accounting).
///
/// When `from_expansion` is `true` the original rustc span was inside a macro
/// expansion, and `lo`/`hi` have been projected to the **callsite**
/// (`Span::source_callsite`) — a real user-file position that is good for
/// display ("generated here") but is **not** an editable `--fix` write surface
/// (the tokens actually live in the macro definition). The production rule is
/// that findings on generated code get no editable span; consumers key that off
/// this flag (for whole-item spans) and off [`ItemFact::vis_span`] being `None`
/// (for the tighten surface). `#[serde(default)]` keeps old fragments loadable.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Span {
    pub file: String,
    pub lo: u32,
    pub hi: u32,
    /// 1-based line of `lo` (`0` only in pre-`line` fragments, which the
    /// vendored-lockstep shipping makes unobservable in practice).
    #[serde(default)]
    pub line: u32,
    #[serde(default)]
    pub from_expansion: bool,
}
