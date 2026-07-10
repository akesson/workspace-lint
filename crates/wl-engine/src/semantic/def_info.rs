//! The def-level vocabulary of the assembly — how a def is categorized,
//! reached, and described. Split from `assembly.rs` so the join logic stays
//! focused (and under the dogfooded file-size cap); `assembly` re-exports
//! everything here, so consumer paths are unchanged.

/// How a def relates to the unused-pub verdict — derived from `parent_kind` +
/// `trait_item` (both rustc-emitted, no text heuristic).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    /// Module-level (parent `mod`/root) — a real unused-pub candidate.
    ModuleLevel,
    /// Inherent-impl item (`impl Foo { .. }`, no trait) — independently-
    /// controllable pub API, judged by its direct-call edges. Also a real
    /// candidate (the syn model can't even see these — a pivot win).
    InherentImpl,
    /// Trait-impl item (`impl Tr for Foo`) — reachable via trait dispatch the
    /// ref graph doesn't edge, visibility forced by the trait. Judged by
    /// dispatch expansion, not direct edges.
    TraitImpl,
    /// Trait *declaration* item, or body-nested / fn-local — excluded.
    Other,
}

impl Category {
    /// Classify from the rustc-emitted `parent_kind` + `trait_item`.
    pub(super) fn of(parent_kind: Option<&str>, trait_item: Option<&str>) -> Self {
        match parent_kind {
            Some("mod") | None => Category::ModuleLevel,
            Some("impl") if trait_item.is_some() => Category::TraitImpl,
            Some("impl") => Category::InherentImpl,
            _ => Category::Other, // trait decl, fn-local, const/static bodies
        }
    }

    /// Is this a def the unused-pub verdict judges? Module-level and
    /// inherent-impl by direct use-site, trait-impl by dispatch expansion.
    /// Only `Other` (trait *declaration* items, fn-local defs) stays out.
    pub(super) fn is_candidate(self) -> bool {
        matches!(
            self,
            Category::ModuleLevel | Category::InherentImpl | Category::TraitImpl
        )
    }
}

/// How a candidate def is reached under *one* config — the per-config
/// primitive the unused-pub verdict and the cfg-matrix union both reduce to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// A real (non-import) use-site references this def directly. (The only
    /// way module-level / inherent-impl items are reached.)
    Direct,
    /// A trait-impl item whose implemented trait is **external** (std/serde/
    /// …). External code dispatches it invisibly (`format!` → `Display::fmt`),
    /// so it can never be proven dead — a sound root, never a lead.
    ExternalDispatch,
    /// A trait-impl item whose implemented trait is **workspace-internal** and
    /// whose trait method is dispatched somewhere (via generic/`dyn`), which
    /// reaches every impl of it.
    InternalDispatch,
    /// Carries an export-shaped attribute (`#[no_mangle]`/`#[export_name]`/
    /// `#[used]`): exported to the linker, no Rust referrer will ever exist —
    /// a sound root, never a lead (the `ffi_no_mangle_export` false positive
    /// this variant retires).
    ExportRoot,
    /// A judged candidate with no reaching edge under this config — a lead.
    Unreached,
}

/// One resolved reference out of a crate's primary-unit code — what
/// `Assembly::references_from` returns and the `architecture` lint judges.
/// `to_path` is canonical: the target's *definition* path for workspace defs
/// (re-export chains resolved), the display path as-referenced otherwise.
#[derive(Debug, Clone)]
pub struct ResolvedRef {
    /// Enclosing module of the use-site (crate-rooted definition path).
    pub module: Vec<String>,
    /// Canonical target path (see type docs).
    pub to_path: Vec<String>,
    /// The target's `DefKind` in the shared vocabulary (best-effort).
    pub to_kind: String,
    /// A `use`/`pub use` declaration, vs a code reference.
    pub import: bool,
    /// A glob import (`use m::*`) — judged as a representative child.
    pub glob: bool,
    /// Local binding name of a single-name import (`use a::B as C` ⇒ `C`).
    pub alias: Option<String>,
    /// Use-site span (file, on-disk byte range, 1-based line); `None` only
    /// for dummy-span edge cases.
    pub span: Option<wl_ir::Span>,
}

/// A def as the reverse index sees it — the owned copy keyed lookups return.
#[derive(Debug)]
pub struct DefInfo {
    pub krate: String,
    /// Display path, `[crate, ..]` joined with `::` — also the cross-config
    /// stable identity the union joins on.
    pub path: String,
    pub kind: String,
    pub public: bool,
    pub category: Category,
    /// For a trait-impl item, the stable key of the trait item it implements —
    /// the dispatch handle. `None` for every non-trait-impl def.
    pub trait_item: Option<String>,
    /// No source span ⇒ a compiler-synthesized def (the `--test` harness's
    /// generated `fn main`, …). Never an unused-pub candidate.
    pub synthetic: bool,
    /// Carries an export-shaped attribute (`ItemFact::attrs` non-empty) — a
    /// linker-visible root; see [`Reach::ExportRoot`].
    pub export_root: bool,
    /// For an inherent-impl item, the stable key of the impl's nominal self
    /// type — the external-reachability handle (see [`wl_ir::ItemFact::self_type`]).
    pub self_type: Option<String>,
    /// The whole-definition span (on-disk byte offsets — see [`wl_ir::Span`]).
    /// `None` exactly when `synthetic`.
    pub span: Option<wl_ir::Span>,
    /// The whole-**item** span — attrs/doc through the body — for the
    /// unused-pub `--fix` deletion surface (see [`wl_ir::ItemFact::full_span`]).
    /// `None` when there is no editable surface (synthetic / macro-generated).
    pub full_span: Option<wl_ir::Span>,
    /// The visibility-token span — the `--fix` tighten write surface. `None`
    /// when there is no independently editable token (private, trait-forced,
    /// macro-generated); see [`wl_ir::ItemFact::vis_span`].
    pub vis_span: Option<wl_ir::Span>,
    /// For an assoc fn: how it takes `self` (see [`wl_ir::ItemFact::self_kind`]).
    pub self_kind: Option<String>,
    /// For an impl-block assoc fn: the self type's `Copy`-ness (see
    /// [`wl_ir::ItemFact::self_copy`]).
    pub self_copy: Option<bool>,
}

/// One candidate def as the cfg-matrix union sees it: reduced to its
/// cross-config stable identity, carrying whether it was reached in *this*
/// config plus the display fields the union verdict reports.
#[derive(Debug)]
pub(crate) struct CandReach {
    pub(crate) reached: bool,
    pub(crate) krate: String,
    pub(crate) category: Category,
    pub(crate) kind: String,
}

/// The kinds the unused-pub verdict judges. Widened from the spike's
/// `fn|struct|enum|trait|type` narrowing (migration PR 4): const/static/macro/
/// union pub items are candidates too — the per-lint `kinds` config filters at
/// the lint layer, not here. Exactly the syn model's `ItemKind::is_definition`
/// set (`mod` deliberately absent — a container, not a named definition).
pub(super) const CANDIDATE_KINDS: &[&str] = &[
    "fn", "struct", "enum", "trait", "type", "const", "static", "macro", "union",
];
