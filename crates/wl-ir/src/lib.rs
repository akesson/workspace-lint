//! The cross-phase IR contract of workspace-lint's rustc-fidelity engine.
//!
//! This is the serialization contract between Phase 1 (the per-crate extractor
//! dylib, which has `TyCtxt`) and Phase 2 (the assembler, which does not). Per
//! `SPIKE-rustc-fidelity-tree.md` §6 this schema is *both* the internal IR and
//! the public extension surface, so it is intentionally plain data. It carries
//! resolved definitions (path, kind, visibility, byte-span) plus the
//! **reference graph** (who-uses-whom) that backs the usage lints.
//!
//! Transport: fragments are serialized as **rkyv** archives and read
//! **zero-copy** — the assembler mmaps each file and accesses the archived
//! types in place (see [`write_bytes`] / [`access_archive`]), so loading a
//! multi-hundred-MB fragment set is a pointer cast, not a parse. serde is
//! retained only for the `dump-ir` debug path, never the hot path.
//!
//! Compatibility model: the rkyv archive is **layout-exact**, so — unlike the
//! former JSON + `#[serde(default)]` scheme — adding a field is NOT
//! backward-compatible: it changes the archived layout, and an old buffer is a
//! different, incompatible type. **Every field addition therefore bumps
//! [`SCHEMA_VERSION`]** (the on-disk header version gate then rejects stale
//! buffers and the completeness guard re-extracts); a change that makes old
//! fragments *misleading* bumps it for the same reason it always did. This is
//! operationally free because the extractor ships vendored in lockstep. The
//! `#[serde(default)]` attributes survive only so `dump-ir` can still read a
//! fragment — they no longer buy transport compatibility.

use serde::{Deserialize, Serialize};

/// The schema version the extractor stamps into every fragment's on-disk
/// header, which loaders assert with [`validate_header`] before any archived
/// access. The extractor and the assembler ship in lockstep (the binary vendors
/// the extractor source), so a mismatch always means a stale cache or a
/// hand-mixed fragment dir — never a supported configuration. A pre-7 `.json`
/// fragment has no header and is rejected the same way (bad magic).
///
/// History: 2 — build-script units emit fragments (`target_kind: "build"`,
/// `<pkg>@build.json`). An old binary reading a shared ir dir containing them
/// would credit build.rs references to `[dependencies]` (its `target_owner`
/// maps `build_script_build` onto an arbitrary member) and prune-thrash the
/// new filenames — silently-misleading, the documented bump trigger.
/// 3 — the unused-pub `--fix` cascade's write surfaces: import edges gained
/// [`RefEdge::decl_span`] / [`RefEdge::elem_span`] (delete dangling `use`s /
/// excise brace-list leaves) and [`ItemFact::full_span`] (the whole-item delete
/// surface — `def_span` alone is only the signature, so a pre-3 fragment would
/// delete a function's header and orphan its body). A pre-3 fragment carries
/// none of them, so the fix would silently skip the import cleanup or leave a
/// broken `use`/body — the misleading-absence the bump forces a re-extract to
/// close.
/// 4 — assoc fns gained [`ItemFact::self_kind`] / [`ItemFact::self_copy`],
/// the substrate for the clippy-unmask guard (narrowing an item strips
/// clippy's `avoid-breaking-exported-api` exemption; `wrong_self_convention`
/// and `len_without_is_empty` then fire on the fixed tree). A pre-4 fragment
/// carries neither, so the guard would silently pass and `--fix` would write
/// a narrow that breaks a `-D warnings` clippy gate.
/// 5 — edges gained [`RefEdge::receiver_resolved`]: `true` for typeck
/// receiver-based resolutions (method calls `x.f()`, field reads `x.f`) that
/// involve no written path. rustc's `unused_imports` only counts written
/// name-resolutions, so the dangling-import check must not let an inherent
/// `.time()` call shield a `use …::TimeView;` whose written users are all
/// deleted. A pre-5 fragment defaults the flag to `false` everywhere, which
/// would re-open exactly that false shield — the bump forces a re-extract.
/// 6 — edges gained [`RefEdge::from_module`]: the `from` item's **lexical**
/// enclosing module. `from` itself is `def_path_str`-rendered, which places an
/// impl member at its self-type's path (`Type::method`, `<Type as
/// Trait>::method`) — the module the impl is *written in* is not recoverable
/// from that text, so import-scope attribution credited a trait-impl body's
/// references to no module at all (and an out-of-module inherent impl's to the
/// wrong one), letting the cascade trim a `use` a surviving trait-method call
/// still needs (E0599 on the fixed tree). A pre-6 fragment carries an empty
/// `from_module`, silently re-opening that hole — the bump forces a re-extract.
/// 7 — transport changed from JSON to **rkyv zero-copy**; the field schema is
/// otherwise identical to 6. The on-disk file gained a 16-byte header
/// (magic + this version + archive length) and a `.wlir` extension (see
/// [`write_bytes`]); a pre-7 `.json` fragment has no header and is rejected at
/// the header gate. Under a layout-exact archive, every *future* field addition
/// must bump this too (see the compatibility model in the module docs) — the
/// additive `#[serde(default)]` tolerance no longer applies to the transport.
/// 8 — the glob-import cleanup substrate: glob `use` edges gained
/// [`RefEdge::decl_span`] (the whole-statement delete surface) and
/// [`RefEdge::glob_used_names`] (the resolver's glob_map — which names
/// actually resolved through this glob), and two new facts landed:
/// [`RefEdge::trait_scope`] (typeck's `used_trait_imports` — a body's method
/// resolution needed this `use` item's target in scope) and
/// [`RefEdge::extern_root`] (the written root bypassed every local import).
/// A pre-8 fragment carries none of them, so the cascade would silently
/// never clean a deletion-orphaned `use m::*;` (an `unused_imports` warning
/// on the fixed tree, a `-D warnings` failure) — the misleading-absence bump
/// trigger, same as bump 3.
/// 9 — fragments gained [`IrFragment::loaded_files`]: every source file rustc
/// actually opened while compiling this unit. It is the substrate of
/// `orphan-file`, which asks "does any declared config compile this file?" —
/// a question no syntactic walker can answer, since the module graph is only
/// determined after macro expansion. A pre-9 fragment carries an empty list,
/// so *every* file would read as never-compiled and the lint would advise
/// deleting the whole workspace — the most destructive possible instance of
/// the misleading-absence bump trigger.
/// 10 — fragments gained [`IrFragment::is_test_cfg`]: whether this unit was
/// compiled with `--test` (rustc's `sess.opts.test` — `cfg(test)` on, `#[test]`
/// fns kept). Until now the `+test` distinction lived only in the fragment's
/// *filename*, erased at load; unused-pub's test-only classification had to
/// infer it from which config saw an edge. A pre-10 fragment defaults the flag
/// to `false`, so a `+test` unit's edges would read as production reach and a
/// test-embalmed `pub` item would silently escape the `TestOnly` verdict — the
/// misleading-absence bump trigger.
/// 11 — no field change; the extractor's signature pass widened to the whole
/// privacy-checked surface: generic bounds and `where`-clauses
/// (`explicit_predicates_of`), supertraits, trait-decl assoc-type and
/// `impl Trait` item bounds, `dyn` principals, and field types now emit
/// [`RefEdge::in_signature`] edges. A pre-11 fragment *under-reports*
/// signature exposure, so unused-pub would keep proposing tightens that are
/// E0445/E0446 on the fixed tree (`pub fn coalesce<R: ByteRange>` was the
/// live instance) — the misleading-absence bump trigger, layout unchanged.
pub const SCHEMA_VERSION: u32 = 11;

/// One crate's contribution to the IR, emitted during that crate's compilation
/// and written to `$WL_IR_OUT/<crate>.wlir`. Phase 2 assembles these.
#[derive(
    Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[rkyv(derive(Debug))]
pub struct IrFragment {
    /// The [`SCHEMA_VERSION`] of the extractor that wrote this fragment. The
    /// on-disk header carries the authoritative copy that [`validate_header`]
    /// gates on before any archived access; this in-archive field mirrors it so
    /// a decoded fragment (`dump-ir`) is self-describing. `#[serde(default)]`
    /// keeps the field readable by the serde debug path.
    #[serde(default)]
    pub schema_version: u32,
    /// Code-form crate name (hyphens → underscores) — the leading segment of
    /// every canonical path, matching the syn resolver's `ResolvedPath[0]`.
    pub crate_name: String,
    /// Which cargo target this fragment was extracted from: `"lib"`, `"bin"`,
    /// `"proc-macro"`, `"test"` (integration test / bench), or `"build"` (a
    /// member's build script — references-only, `items` empty). Cargo allows
    /// a package's bin target to share the lib's crate name (`src/lib.rs` +
    /// `src/main.rs`), so `crate_name` alone cannot key a fragment — without
    /// this the bin fragment used to *clobber* the lib's on disk and lib-only
    /// deps read as unused. Also the primary-units signal for the
    /// `architecture` lint (test and build targets legitimately reach across
    /// layers). `""` in pre-field fragments (unobservable in practice — the
    /// extractor ships vendored in lockstep).
    #[serde(default)]
    pub target_kind: String,
    /// This unit was compiled with `--test` (rustc's `sess.opts.test`):
    /// `cfg(test)` is on and `#[test]` fns are kept for the harness. Together
    /// with [`target_kind`](Self::target_kind) this totally classifies a
    /// unit's provenance — a `+test` lib/bin variant keeps its plain
    /// `target_kind`, so without this flag it is indistinguishable from the
    /// production unit once the `+test` filename suffix is gone. A unit is
    /// *test code* iff `target_kind == "test"` (integration test / bench) or
    /// this flag is set; that split is what backs unused-pub's "only used by
    /// test code" verdict.
    #[serde(default)]
    pub is_test_cfg: bool,
    pub items: Vec<ItemFact>,
    /// Resolved reference edges harvested from this crate's HIR: a `from` local
    /// item *uses* a `to` def (local or cross-crate). This is the reference graph
    /// that will back the usage lints (`unused-pub`, `unused-deps`, architecture
    /// rules) — rustc's resolved answer to syn's text-based occurrence model.
    /// Deduped and sorted so the fragment is deterministic (byte-identical across
    /// driver/dylib). `#[serde(default)]` keeps pre-references fragments loadable.
    #[serde(default)]
    pub references: Vec<RefEdge>,
    /// Every source file rustc opened while compiling this unit, restricted to
    /// this crate's `CARGO_MANIFEST_DIR` (registry deps and `OUT_DIR` are
    /// dropped) and stored as absolute, canonicalized paths. Deduped and sorted.
    ///
    /// This is rustc's own answer to "which files are part of this crate",
    /// which is the only sound answer: `#[cfg_attr(unix, path = "…")]`, an
    /// `include!` in expression position, a `macro_rules!`-generated `mod`, and
    /// a build-script-computed include path are all resolved here for free,
    /// where a syntactic walker must re-implement macro expansion to see them.
    ///
    /// Scoped per *compilation unit*, so it is cfg-dependent by construction: a
    /// `#[cfg(test)] mod tests;` in its own file appears only in the `+test`
    /// fragment. Consumers must union across the `[engine]` config matrix.
    #[serde(default)]
    pub loaded_files: Vec<String>,
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
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[rkyv(derive(Debug))]
pub struct RefEdge {
    pub from: Vec<String>,
    pub to: Vec<String>,
    /// Def path of the `from` item's **lexical** enclosing module
    /// (`[crate_code, mods…]`; the crate root is `[crate_code]`). `from` is
    /// `def_path_str`-rendered, which places an impl member at its self-type's
    /// path (`Type::method`, `<Type as Trait>::method`) — the module the impl
    /// is *written in* is not a textual prefix of that rendering. rustc
    /// resolves a body's names through the imports of the module the code is
    /// lexically in, so import-scope attribution needs this as its own fact.
    /// Empty only on pre-6 fragments.
    #[serde(default)]
    pub from_module: Vec<String>,
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
    /// `true` iff this edge came from a typeck **receiver-based** resolution —
    /// a method call (`x.f()`) or a field read (`x.f`) — rather than a written
    /// path. Such a use involves no name resolution through any `use`
    /// statement (rustc resolves it from the receiver's type), so the
    /// dangling-import check must not count it as keeping an import alive.
    /// The one exception is handled downstream: a *trait* member still
    /// requires its trait in scope, so trait-member edges credit the trait's
    /// import regardless of this flag. Written `Type::assoc` paths
    /// (`QPath::TypeRelative`) are `false` — they do resolve `Type` through
    /// its import.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub receiver_resolved: bool,
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
    /// `true` iff this edge came from the *signature-position* walk rather
    /// than a body/path use-site: the lowered types (`fn_sig`/`type_of`,
    /// incl. field types and generic-parameter defaults) plus the written
    /// predicate surface (bounds/`where`-clauses, supertraits, assoc-type and
    /// `impl Trait` item bounds, `dyn` principals). Backs
    /// `exposed_in_public_signature`: tightening a def that appears in a pub
    /// item's signature would break compilation (E0445/E0446 /
    /// `private_interfaces`), so the unused-pub `--fix` must not propose it.
    /// Field-type edges carry the FIELD as `from`, so exposure gates on the
    /// field's own visibility.
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
    /// For a **glob** `import` edge: the names rustc's resolver recorded as
    /// actually resolved *through this glob* (`names_imported_by_glob_use` —
    /// the resolver's glob_map, the same fact rustc's `unused_imports` and
    /// clippy's `wildcard_imports` consult), sorted for determinism. Empty ⇒
    /// the resolver saw no use of the glob at all. This is the
    /// rendering-independent name evidence the glob-dangling accounting is
    /// built on: display paths for the same def can diverge
    /// (`dioxus_core::Element` vs `dioxus::prelude::Element`), final-segment
    /// names cannot.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub glob_used_names: Vec<String>,
    /// `true` iff this edge records "some body owned by `from` needed the
    /// target's `use` item in scope for **method resolution**" (typeck's
    /// `used_trait_imports`). For a single-name `use` the target is the
    /// trait; for a glob it is the glob's module. NOT a written path:
    /// consumers must segregate it from name-resolution evidence (it exists
    /// so a glob kept alive only by trait-method syntax — invisible to both
    /// written paths and the glob_map — is still visibly load-bearing).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub trait_scope: bool,
    /// `true` iff the written path's first segment resolved to an **extern
    /// crate root** — the resolution bypassed every local `use`. Generalizes
    /// [`RefEdge::via`] (which is only set when the written root differs from
    /// the defining crate). The glob-dangling belt-and-braces rule uses it to
    /// ignore survivors that cannot have resolved through any import.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub extern_root: bool,
    /// The extern crate named by the path's first segment *as written*, when
    /// it differs from the crate defining the resolved target: `use
    /// shim::Item` where `shim` merely re-exports another crate's `Item`.
    /// rustc name-resolution follows the re-export chain, so `to[0]` names
    /// the *defining* crate (`std` for `web_time::Instant` on a non-wasm
    /// target) — without this field a dependency used only through its
    /// re-exports of another crate's items is invisible to `unused-deps`.
    /// `None` when the written root is the defining crate, a local path, or
    /// a keyword root (`crate`/`self`/`super`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    /// The use-site: where in `from`'s source this reference occurs. `None`
    /// for lowered-signature-pass edges (no single HIR token) and dummy
    /// spans. For a macro-generated reference this is the *invocation site*
    /// (`Span::from_expansion` set). Kept ahead of the two import-only spans so
    /// the derived `Ord` compares edge identity, then the use-site, first; the
    /// extractor dedups on identity alone (see `edge_identity`) and keeps the
    /// first (lowest) span — five calls to the same def are still one edge,
    /// anchored at the earliest use-site.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<Span>,
    /// For an `import` edge: the span of the **whole `use …;` declaration** the
    /// leaf belongs to (the enclosing HIR `use` item). Every leaf lowered from
    /// one source declaration shares it, so it is the grouping key the
    /// unused-pub `--fix` uses to tell a sole-leaf import (delete the whole
    /// statement) from a brace-list leaf (excise just the leaf). A **glob**
    /// edge carries it too (its whole-statement delete surface; a glob has no
    /// `elem_span`, so [`RefEdge::glob`] is its discriminator). `None` for
    /// non-import edges and macro-generated `use`s (no editable declaration).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decl_span: Option<Span>,
    /// For an `import` edge: the span of **this leaf as written** — the final
    /// path segment through any `as`-rename binding (`b` in `use a::{b, c}`,
    /// `B as C` in `use a::B as C`). The intra-brace write surface: excising it
    /// (plus one adjacent separator) removes the leaf while leaving live
    /// siblings — including ones importing out-of-workspace items the assembler
    /// never sees. `None` for non-import edges, globs (no single leaf), and
    /// macro-generated `use`s.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elem_span: Option<Span>,
}

/// A single resolved definition.
#[derive(
    Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[rkyv(derive(Debug))]
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
    ///
    /// This is rustc's `def_span` — the **signature/header** span (`pub fn f()
    /// -> T`, `pub struct S`), the natural diagnostic-anchor line. It is NOT
    /// the deletion surface: deleting it would orphan a function's body. Use
    /// [`Self::full_span`] to remove the whole item.
    pub span: Option<Span>,
    /// The **whole-item** span for the unused-pub `--fix` deletion surface:
    /// leading doc comments and attributes through the closing brace of the
    /// body (`span_with_body`, extended over the item's attribute spans).
    /// Deleting this leaves no orphaned body block or dangling doc comment.
    /// `None` for synthetic/macro-generated defs (no editable surface — same
    /// condition as [`Self::span`] being `None` or from-expansion).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_span: Option<Span>,
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
    /// For an **assoc fn**: how it takes `self` — `"none"` (no receiver, or
    /// an explicit `self: Box<Self>`-style receiver the HIR doesn't model as
    /// implicit), `"value"` (`self` / `mut self`), `"ref"` (`&self`), or
    /// `"ref_mut"` (`&mut self`). `None` for every non-assoc-fn def. The
    /// clippy-unmask guard replays `wrong_self_convention`'s naming table
    /// against it before `--fix` narrows an item out of clippy's
    /// `avoid-breaking-exported-api` exemption.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_kind: Option<String>,
    /// For an assoc fn in an **impl block**: whether the impl's self type is
    /// `Copy` — clippy's convention table accepts by-value `self` where a
    /// reference is otherwise expected (and *expects* it for `to_*`) on
    /// `Copy` types. `None` for trait-declaration items (generic `Self`,
    /// unknowable) and non-assoc defs; the guard treats `None` as not-`Copy`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_copy: Option<bool>,
}

/// Normalized visibility. `Restricted` carries the rendered restriction
/// (`"crate"`, or a module path) so the diff against syn's
/// `Public`/`PubCrate`/`PubSuper`/`PubIn`/`Private` can normalize later.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[rkyv(derive(Debug), compare(PartialEq))]
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
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[rkyv(derive(Debug))]
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

// ---------------------------------------------------------------------------
// rkyv transport: the on-disk fragment format (schema 7+).
//
// File layout: a 16-byte header, then the rkyv archive bytes (root at the end
// of the archive slice). The header is validated *before* any archived access,
// so a stale/foreign/truncated file is rejected loudly instead of casting into
// garbage. 16 bytes keeps the archive slice at a 16-aligned offset under the
// page-aligned mmap base — above `ArchivedIrFragment`'s max alignment (4).
// ---------------------------------------------------------------------------

/// The 4-byte magic every fragment file starts with.
pub const MAGIC: [u8; 4] = *b"WLIR";

/// Header length in bytes: `MAGIC` (4) + `SCHEMA_VERSION` (u32) + archive
/// length (u64). Also the offset at which the rkyv archive begins.
pub const HEADER_LEN: usize = 16;

/// Why a byte buffer is not a valid current-schema fragment. Every variant is
/// the loud-rejection path the completeness guard turns into a re-extract —
/// none is a silent skew.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeaderError {
    /// Fewer than [`HEADER_LEN`] bytes, or no room for an `ArchivedIrFragment`.
    TooShort,
    /// First four bytes are not [`MAGIC`] — not a `.wlir` fragment (e.g. a
    /// stale pre-7 `.json` file, or an unrelated file).
    BadMagic,
    /// Written by a different [`SCHEMA_VERSION`] (stale cache / foreign build).
    SchemaMismatch { found: u32 },
    /// The recorded archive length disagrees with the file — a truncated or
    /// partially-written buffer.
    LenMismatch,
}

impl std::fmt::Display for HeaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooShort => write!(f, "fragment is too short to hold a header + archive"),
            Self::BadMagic => write!(
                f,
                "not a workspace-lint IR fragment (bad magic) — a stale `.json` fragment \
                 or foreign file; delete it and re-extract"
            ),
            Self::SchemaMismatch { found } => write!(
                f,
                "IR fragment has schema version {found} but this build expects {SCHEMA_VERSION}; \
                 the fragment dir is stale or was written by a different extractor build \
                 — delete it and re-extract"
            ),
            Self::LenMismatch => write!(
                f,
                "IR fragment length is inconsistent with its header (truncated or \
                 partially written) — delete it and re-extract"
            ),
        }
    }
}

impl std::error::Error for HeaderError {}

/// Serialize a fragment to its full on-disk bytes: the [`HEADER_LEN`]-byte
/// header followed by the rkyv archive. The extractor writes this (temp file +
/// atomic rename); the assembler mmaps it and calls [`access_archive`].
pub fn write_bytes(fragment: &IrFragment) -> Result<Vec<u8>, rkyv::rancor::Error> {
    let archive = rkyv::to_bytes::<rkyv::rancor::Error>(fragment)?;
    let mut out = Vec::with_capacity(HEADER_LEN + archive.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&SCHEMA_VERSION.to_le_bytes());
    out.extend_from_slice(&(archive.len() as u64).to_le_bytes());
    out.extend_from_slice(&archive);
    Ok(out)
}

/// The bare rkyv archive of a fragment (no header) — the in-memory
/// representation the fixture/test path uses (`FragmentBytes::Owned`).
pub fn to_archive(fragment: &IrFragment) -> Result<rkyv::util::AlignedVec, rkyv::rancor::Error> {
    rkyv::to_bytes::<rkyv::rancor::Error>(fragment)
}

/// Validate a full on-disk fragment buffer's header. Call this before
/// [`access_archive`] — it is what makes the subsequent unchecked access sound
/// for stale/foreign/truncated inputs. Returns the archive-bytes offset range's
/// implicit start ([`HEADER_LEN`]) via `Ok`; the caller slices `&buf[HEADER_LEN..]`.
pub fn validate_header(buf: &[u8]) -> Result<(), HeaderError> {
    if buf.len() < HEADER_LEN {
        return Err(HeaderError::TooShort);
    }
    if buf[0..4] != MAGIC {
        return Err(HeaderError::BadMagic);
    }
    let found = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
    if found != SCHEMA_VERSION {
        return Err(HeaderError::SchemaMismatch { found });
    }
    let archive_len = u64::from_le_bytes(buf[8..16].try_into().expect("8-byte slice")) as usize;
    if archive_len != buf.len() - HEADER_LEN {
        return Err(HeaderError::LenMismatch);
    }
    if archive_len < std::mem::size_of::<ArchivedIrFragment>() {
        return Err(HeaderError::TooShort);
    }
    Ok(())
}

/// Access the archived fragment inside a validated on-disk buffer, zero-copy.
///
/// # Safety
/// `buf` must have passed [`validate_header`] in this build (magic, current
/// [`SCHEMA_VERSION`], and length all check out) and be otherwise intact — i.e.
/// produced by [`write_bytes`] of this exact schema. The header gate covers the
/// stale/foreign/truncated cases; the residual precondition is no mid-buffer
/// corruption. For a corruption-diagnosis path use [`access_archive_checked`].
pub unsafe fn access_archive(buf: &[u8]) -> &ArchivedIrFragment {
    // SAFETY: contract delegated to the caller (header validated); the archive
    // slice starts at HEADER_LEN, which is 16-aligned under a page-aligned mmap
    // base — satisfying the archive's alignment (<= 4).
    unsafe { rkyv::access_unchecked::<ArchivedIrFragment>(&buf[HEADER_LEN..]) }
}

/// Checked counterpart of [`access_archive`]: validates every relative pointer,
/// bound, and UTF-8 string in the archive. O(n) over the whole buffer (pages it
/// all in) — the `WL_IR_CHECKED` diagnosis path only, never the hot path.
pub fn access_archive_checked(buf: &[u8]) -> Result<&ArchivedIrFragment, rkyv::rancor::Error> {
    rkyv::access::<ArchivedIrFragment, rkyv::rancor::Error>(&buf[HEADER_LEN..])
}

/// Reconstruct an owned [`IrFragment`] from a bare archive slice (no header) —
/// the `dump-ir` debug path and the test round-trip. Checked (validates then
/// deserializes); not for the hot path.
pub fn from_archive_bytes(archive: &[u8]) -> Result<IrFragment, rkyv::rancor::Error> {
    rkyv::from_bytes::<IrFragment, rkyv::rancor::Error>(archive)
}

/// Join archived (or native) path segments with `sep`. Archived string vectors
/// have no `join`, and the assembler joins `ItemFact::path` / `RefEdge::{to,from}`
/// constantly; generic over `AsRef<str>` so native fixtures and archived runtime
/// share one implementation.
pub fn join_paths<S: AsRef<str>>(segs: &[S], sep: &str) -> String {
    let mut out = String::new();
    for (i, s) in segs.iter().enumerate() {
        if i > 0 {
            out.push_str(sep);
        }
        out.push_str(s.as_ref());
    }
    out
}

impl From<&ArchivedSpan> for Span {
    /// Materialize an owned [`Span`] from its archived view — the assembler
    /// stores owned spans in `DefInfo`/`ResolvedRef`/`DanglingImport`.
    fn from(a: &ArchivedSpan) -> Self {
        Span {
            file: a.file.as_str().to_owned(),
            lo: a.lo.to_native(),
            hi: a.hi.to_native(),
            line: a.line.to_native(),
            from_expansion: a.from_expansion,
        }
    }
}

#[cfg(test)]
mod transport_tests {
    use super::*;

    fn sample() -> IrFragment {
        IrFragment {
            schema_version: SCHEMA_VERSION,
            crate_name: "demo".into(),
            target_kind: "lib".into(),
            is_test_cfg: false,
            loaded_files: vec!["src/lib.rs".into()],
            items: vec![ItemFact {
                path: vec!["demo".into(), "thing".into()],
                key: "deadbeef".into(),
                kind: "fn".into(),
                parent_kind: Some("mod".into()),
                trait_item: None,
                self_type: None,
                visibility: Visibility::Public,
                span: Some(Span {
                    file: "src/lib.rs".into(),
                    lo: 10,
                    hi: 42,
                    line: 2,
                    from_expansion: false,
                }),
                full_span: None,
                vis_span: None,
                attrs: vec![],
                self_kind: None,
                self_copy: None,
            }],
            references: vec![],
        }
    }

    #[test]
    fn round_trip_zero_copy() {
        let frag = sample();
        let buf = write_bytes(&frag).expect("serialize");
        validate_header(&buf).expect("header valid");
        // SAFETY: buf is from write_bytes of this schema, header just validated.
        let archived = unsafe { access_archive(&buf) };
        assert_eq!(archived.crate_name.as_str(), "demo");
        assert_eq!(archived.items.len(), 1);
        assert_eq!(join_paths(&archived.items[0].path, "::"), "demo::thing");
        assert!(archived.items[0].visibility == Visibility::Public);
        let span = Span::from(archived.items[0].span.as_ref().expect("span"));
        assert_eq!(span.lo, 10);
        assert_eq!(span.hi, 42);
    }

    #[test]
    fn header_rejects_bad_magic_and_version() {
        let mut buf = write_bytes(&sample()).expect("serialize");
        // Corrupt magic → BadMagic.
        let mut bad = buf.clone();
        bad[0] = b'X';
        assert_eq!(validate_header(&bad), Err(HeaderError::BadMagic));
        // Wrong version → SchemaMismatch.
        buf[4] = buf[4].wrapping_add(1);
        assert!(matches!(
            validate_header(&buf),
            Err(HeaderError::SchemaMismatch { .. })
        ));
    }

    #[test]
    fn checked_access_matches() {
        let buf = write_bytes(&sample()).expect("serialize");
        let archived = access_archive_checked(&buf).expect("checked access");
        assert_eq!(archived.crate_name.as_str(), "demo");
    }

    /// Guards the compatibility model: the rkyv archive is layout-exact, so any
    /// field add/remove/reorder that changes an archived type's size is a
    /// transport break. If this fails, you changed the on-disk layout — bump
    /// [`SCHEMA_VERSION`] (so stale caches are rejected at the header gate) and
    /// update these pins in the same commit. A same-size reorder still needs
    /// the version bump even though this test wouldn't catch it — the pins are
    /// a tripwire, not a proof.
    #[test]
    fn archived_layout_is_pinned() {
        use std::mem::size_of;
        assert_eq!(
            size_of::<ArchivedIrFragment>(),
            48,
            "ArchivedIrFragment layout changed"
        );
        assert_eq!(
            size_of::<ArchivedItemFact>(),
            180,
            "ArchivedItemFact layout changed"
        );
        assert_eq!(
            size_of::<ArchivedRefEdge>(),
            176,
            "ArchivedRefEdge layout changed"
        );
        assert_eq!(size_of::<ArchivedSpan>(), 24, "ArchivedSpan layout changed");
    }
}
