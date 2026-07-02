//! Phase-1 rustc-fidelity IR extractor: a Dylint `LateLintPass` that walks the
//! `TyCtxt` of the crate under compilation and emits a byte-precise
//! [`IrFragment`] (SPIKE-rustc-fidelity-tree.md §4/§11).
//!
//! This "lint" never warns: it harvests facts into `$WL_IR_OUT/<crate>.json`
//! (the IR channel). Real diagnostics (the findings channel) ride Dylint's
//! native lint path separately; the two never mix (SPIKE §4). The
//! findings-channel round-trip itself was proven by the spike (WS1-A4, SPIKE
//! §12.6/§12b) with a demo lint that did not graduate with this package.
//!
//! The spike's raw `rustc_driver` twin (`spike/driver`) carries an identical
//! copy of [`extract`] — the original proof that the walk is host-agnostic.
#![feature(rustc_private)]

// Compiler-crate imports are inherited from the `cargo dylint new` template:
// this exact set is known to resolve on the pinned toolchain. We use rustc_hir /
// _middle / _span / _lint directly; the rest are transitive needs. The library
// hand-writes `register_lints` instead of using `declare_late_lint!` (the
// production shape: findings lints join the same registration later) — which
// means `rustc_lint` / `rustc_session` are declared here explicitly (the macro
// used to inject them), and `dylint_library!()` supplies the dylib glue.
extern crate rustc_arena;
extern crate rustc_ast;
extern crate rustc_ast_pretty;
extern crate rustc_data_structures;
extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_hir_pretty;
extern crate rustc_index;
extern crate rustc_infer;
extern crate rustc_lexer;
extern crate rustc_lint;
extern crate rustc_middle;
extern crate rustc_mir_dataflow;
extern crate rustc_parse;
extern crate rustc_session;
extern crate rustc_span;
extern crate rustc_target;
extern crate rustc_trait_selection;

use rustc_hir::def::DefKind;
use rustc_hir::def_id::{CRATE_DEF_ID, DefId, LOCAL_CRATE, LocalDefId};
use rustc_hir::intravisit::{self, Visitor};
use rustc_hir::{Expr, ExprKind, HirId, Path, QPath, UsePath};
use rustc_lint::{LateContext, LateLintPass, LintStore};
use rustc_middle::hir::nested_filter;
use rustc_middle::ty::{self, Ty, TyCtxt, TypeSuperVisitable, TypeVisitable, TypeVisitor};
use rustc_session::Session;
use rustc_span::{FileName, Span as RustcSpan};

use wl_ir::{IrFragment, ItemFact, RefEdge, Span, Visibility};

// The dylib entry glue `declare_late_lint!` used to supply: the `dylint_version`
// export + `extern crate rustc_driver`. Required for the dylib to load.
dylint_linting::dylint_library!();

rustc_session::declare_lint! {
    /// ### What it does
    ///
    /// Walks `TyCtxt` for the crate under compilation and writes a byte-precise
    /// IR fragment to `$WL_IR_OUT/<crate>.json`. Phase 1 of the rustc-fidelity
    /// pivot (SPIKE-rustc-fidelity-tree.md §11.2), repackaged from the raw
    /// `rustc_driver` spike into a Dylint pass.
    ///
    /// ### Why is this bad?
    ///
    /// It isn't — this pass never emits a diagnostic. It exists to harvest the
    /// IR (**facts channel**). Real findings ride a separate channel (SPIKE §4).
    pub WL_IR_EXTRACT,
    // MUST be Warn+ (not Allow): rustc does NOT schedule a LateLintPass whose
    // lints are all Allow, so an Allow-level extractor's `check_crate` never
    // runs and no IR is emitted (verified: SPIKE §12.4 finding). The pass stays
    // silent because it never calls a `span_lint`; Warn is just the switch that
    // makes rustc run it.
    Warn,
    "harvests the rustc-fidelity IR fragment for the crate under compilation"
}

// Hand-written `register_lints` (not the single-lint `declare_late_lint!`
// macro): this is the production registration shape — findings-channel lints
// join the same `LintStore` later (SPIKE §8: standard Dylint lints run in the
// same pass, one compilation, one `TyCtxt`, both channels).
#[allow(clippy::no_mangle_with_rust_abi)]
#[unsafe(no_mangle)]
pub fn register_lints(sess: &Session, lint_store: &mut LintStore) {
    dylint_linting::init_config(sess);
    lint_store.register_lints(&[WL_IR_EXTRACT]);
    // NOTE (WS2 treadmill, SPIKE §12.4): on the 04-16 pin `register_late_pass` takes
    // a bare `impl Fn(TyCtxt) -> LateLintPassObject`, so the closure's `Box::new(..)`
    // return coerces to `Box<dyn LateLintPass>`. On nightly-2026-06-25 the param
    // became a boxed `LateLintPassFactory`, needing `register_late_pass(Box::new(|_|
    // Box::new(WlIrExtract) as _))` — a real, toolchain-specific edit (no single
    // spelling straddles both pins: the plain closure fails to box on 06-25; the
    // boxed form's concrete return type fails to coerce on 04-16). This was the ONLY
    // extractor breakage across the ~10-week jump; dylint_linting 6.0.1 itself
    // compiled unchanged. Keep the 04-16 form here (the production pin).
    lint_store.register_late_pass(|_| Box::new(WlIrExtract));
}

rustc_session::declare_lint_pass!(WlIrExtract => [WL_IR_EXTRACT]);

impl<'tcx> LateLintPass<'tcx> for WlIrExtract {
    // The lift point: `cx.tcx` is the same `TyCtxt` the raw driver walked in
    // `after_analysis`. Everything below is `extract()` moved verbatim.
    fn check_crate(&mut self, cx: &LateContext<'tcx>) {
        // A member's build script compiles as its own crate, and cargo names
        // EVERY one `build_script_build` — so a workspace's build scripts would
        // all collide on one fragment filename, and none is a lintable target.
        // Skip them (deferred-gap ledger: keying on CARGO_PKG_NAME would let a
        // future build-dep-usage analysis keep these).
        if cx.tcx.crate_name(LOCAL_CRATE).as_str() == "build_script_build" {
            return;
        }
        let fragment = extract(cx.tcx);
        // `--test` mode compiles a distinct crate variant (cfg(test) on, #[test]
        // fns kept for the harness). It can coexist with the plain-lib build of
        // the same crate, which would race on one filename — so key the output on
        // it. Lets the fidelity oracle compare config-matched IRs (SPIKE §7/§10).
        write_fragment(&fragment, cx.tcx.sess.opts.test);
    }
}

/// Walk every local definition and project it into the IR. Verbatim from the
/// raw driver's `extract()` — the whole point of the spike is that this body is
/// identical whether driven by `rustc_driver` or by Dylint.
fn extract(tcx: TyCtxt<'_>) -> IrFragment {
    let crate_code = tcx.crate_name(LOCAL_CRATE).to_string().replace('-', "_");
    let sm = tcx.sess.source_map();
    let mut items = Vec::new();

    for local_id in tcx.hir_crate_items(()).definitions() {
        let def_id = local_id.to_def_id();
        let kind = match tcx.def_kind(def_id) {
            DefKind::Struct => "struct",
            DefKind::Enum => "enum",
            DefKind::Union => "union",
            DefKind::Trait => "trait",
            DefKind::TraitAlias => "trait_alias",
            DefKind::TyAlias => "type",
            DefKind::Fn | DefKind::AssocFn => "fn",
            DefKind::Const { .. } | DefKind::AssocConst { .. } => "const",
            DefKind::Static { .. } => "static",
            DefKind::Mod => "mod",
            DefKind::Macro(_) => "macro",
            _ => continue, // closures, impls, params, ctors, … — not tree items
        };

        // def_path_str omits the crate for local items; prepend the code name.
        let mut path = vec![crate_code.clone()];
        let rel = tcx.def_path_str(def_id);
        if !rel.is_empty() {
            path.extend(rel.split("::").map(str::to_string));
        }

        let visibility = match tcx.visibility(def_id) {
            ty::Visibility::Public => Visibility::Public,
            ty::Visibility::Restricted(m) => {
                if m == CRATE_DEF_ID.to_def_id() {
                    Visibility::Restricted("crate".to_string())
                } else {
                    Visibility::Restricted(tcx.def_path_str(m))
                }
            }
        };

        items.push(ItemFact {
            path,
            key: def_key(tcx, def_id),
            kind: kind.to_string(),
            parent_kind: parent_def_kind(tcx, def_id),
            trait_item: trait_item_key(tcx, def_id),
            visibility,
            span: span_to_ir(sm, tcx.def_span(def_id)),
            vis_span: vis_span_to_ir(tcx, sm, local_id),
            attrs: export_attrs(tcx, local_id),
        });
    }

    let references = collect_references(tcx, &crate_code);

    IrFragment {
        schema_version: wl_ir::SCHEMA_VERSION,
        crate_name: crate_code,
        items,
        references,
    }
}

/// Harvest the crate's reference graph by walking the whole HIR and resolving
/// every name-resolved path to the def it points at, attributed to the nearest
/// enclosing item (`from`). Deduped + sorted so the fragment is deterministic.
///
/// Coverage: name-resolved paths (`Res`-carrying — function/type/trait/ADT
/// references, imports), method calls `x.f()`, and type-relative **value** paths
/// `Type::assoc_fn` / `Type::CONST` (via the enclosing body's `typeck`). A second
/// pass ([`RefCollector::collect_signature_projections`]) then adds **type-position
/// assoc projections** (`<T as Trait>::Item`) from lowered signatures. Remaining
/// gap: projections that appear *only* in bounds / `where`-clauses (need
/// `predicates_of`), and opaque/`impl Trait` targets — later increments.
fn collect_references(tcx: TyCtxt<'_>, crate_code: &str) -> Vec<RefEdge> {
    let mut collector = RefCollector {
        tcx,
        crate_code,
        edges: Vec::new(),
    };
    tcx.hir_walk_toplevel_module(&mut collector);
    collector.collect_signature_projections();
    let mut edges = collector.edges;
    edges.sort();
    edges.dedup();
    edges
}

struct RefCollector<'a, 'tcx> {
    tcx: TyCtxt<'tcx>,
    crate_code: &'a str,
    edges: Vec<RefEdge>,
}

impl<'a, 'tcx> RefCollector<'a, 'tcx> {
    /// Record `enclosing-item-of(at) → to`. `import` marks the edge as a
    /// `use`/re-export rather than a use-site (see [`RefEdge::import`]).
    fn record(&mut self, at: HirId, to: DefId, import: bool) {
        let from_id = self.tcx.hir_get_parent_item(at).to_def_id();
        self.record_edge(from_id, to, import, false);
    }

    /// Record `from_id → to`, skipping self-edges. Two flavours of self-edge:
    /// same-`DefId` (an item's own def in its signature/body — noise, not usage),
    /// and path-level (`def_path_str` renders an `impl T` block by its self-type,
    /// so a ref to `T` from inside its own impl collapses to from==to).
    /// `in_signature` marks edges from the lowered-signature pass (the HIR walk
    /// may emit the same reference unflagged; consumers of the flag are
    /// boolean, so the near-duplicate is harmless and keeps both provenances).
    fn record_edge(&mut self, from_id: DefId, to: DefId, import: bool, in_signature: bool) {
        if from_id == to {
            return;
        }
        let from = local_path(self.tcx, self.crate_code, from_id);
        let external = to.krate != LOCAL_CRATE;
        let to_path = if external {
            ext_path(self.tcx, to)
        } else {
            local_path(self.tcx, self.crate_code, to)
        };
        if from == to_path {
            return;
        }
        self.edges.push(RefEdge {
            from,
            to: to_path,
            from_key: def_key(self.tcx, from_id),
            to_key: def_key(self.tcx, to),
            to_kind: def_kind_str(self.tcx.def_kind(to)),
            external,
            import,
            in_signature,
        });
    }

    /// Second pass over each item's *lowered* signature (`fn_sig`/`type_of` —
    /// cached queries, no empty-env ICE), with two jobs:
    ///
    /// 1. Associated-type projections in *type* position (`<S as Tr>::Item`,
    ///    `T::Item`, `Self::Out`) — the one reference class the HIR walk can't
    ///    resolve (their `PathSegment::res` is `Res::Err`; resolution is
    ///    deferred past name-resolution).
    /// 2. Every *named type* (ADTs, foreign types) in the signature, flagged
    ///    [`RefEdge::in_signature`] — the substrate of
    ///    `exposed_in_public_signature` (a type named in a pub signature must
    ///    not be visibility-tightened: E0446 / `private_interfaces`).
    fn collect_signature_projections(&mut self) {
        for local in self.tcx.hir_crate_items(()).definitions() {
            let did = local.to_def_id();
            let mut sig = SigVisitor { out: Vec::new() };
            // `skip_binder` (not `instantiate_identity`) keeps aliases *un-normalized*
            // — we want the projection, not what it resolves to.
            match self.tcx.def_kind(did) {
                DefKind::Fn | DefKind::AssocFn => {
                    self.tcx.fn_sig(did).skip_binder().visit_with(&mut sig);
                }
                DefKind::TyAlias
                | DefKind::Const { .. }
                | DefKind::AssocConst { .. }
                | DefKind::Static { .. } => {
                    self.tcx.type_of(did).skip_binder().visit_with(&mut sig);
                }
                _ => continue,
            }
            for to in sig.out {
                self.record_edge(did, to, false, true);
            }
        }
    }
}

/// Collects the `def_id` of every type *named* in a `ty::Ty` tree: assoc-type
/// projections (whose `def_id` lives in the `AliasTyKind` variant) and plain
/// ADTs. Both feed the same edge stream; the ADTs additionally carry the
/// signature-position flag's meaning for `exposed_in_public_signature`.
struct SigVisitor {
    out: Vec<DefId>,
}

impl<'tcx> TypeVisitor<TyCtxt<'tcx>> for SigVisitor {
    fn visit_ty(&mut self, t: Ty<'tcx>) {
        match t.kind() {
            ty::Alias(alias) => {
                if let ty::AliasTyKind::Projection { def_id }
                | ty::AliasTyKind::Inherent { def_id } = alias.kind
                {
                    self.out.push(def_id);
                }
            }
            ty::Adt(adt, _) => self.out.push(adt.did()),
            _ => {}
        }
        t.super_visit_with(self);
    }
}

impl<'a, 'tcx> Visitor<'tcx> for RefCollector<'a, 'tcx> {
    // Descend into nested items and bodies — references live in fn bodies, type
    // positions, and initializers, not just top-level signatures.
    type NestedFilter = nested_filter::All;

    fn maybe_tcx(&mut self) -> TyCtxt<'tcx> {
        self.tcx
    }

    fn visit_use(&mut self, path: &'tcx UsePath<'tcx>, hir_id: HirId) {
        // `use` / `pub use` — record each namespace resolution as an **import**
        // edge (`RefEdge::import`), not a use-site. A `UsePath` resolves in up to
        // three namespaces (`PerNS`); record every present one. We deliberately
        // don't call `walk_use`: its default reconstructs a `Path` per namespace
        // and re-drives `visit_path`, which would re-record these as non-import
        // edges (their enclosing item is the *module*, so `record` couldn't tell
        // them apart). `walk_item` still visits the `Single` ident separately.
        for res in path.res.present_items() {
            if let Some(to) = res.opt_def_id() {
                self.record(hir_id, to, true);
            }
        }
    }

    fn visit_path(&mut self, path: &Path<'tcx>, id: HirId) {
        if let Some(to) = path.res.opt_def_id() {
            self.record(id, to, false);
        }
        intravisit::walk_path(self, path);
    }

    fn visit_expr(&mut self, ex: &'tcx Expr<'tcx>) {
        // Type-dependent resolutions path `Res` can't see — both read the
        // enclosing body's typeck results:
        //   * method calls `x.f()`                    (MethodCall)
        //   * type-relative value paths `Type::g`/`Type::CONST`
        //     (Path(QPath::TypeRelative), via `qpath_res`)
        match ex.kind {
            ExprKind::MethodCall(..) => {
                let owner = self.tcx.hir_enclosing_body_owner(ex.hir_id);
                if let Some(to) = self.tcx.typeck(owner).type_dependent_def_id(ex.hir_id) {
                    self.record(ex.hir_id, to, false);
                }
            }
            ExprKind::Path(ref qpath @ QPath::TypeRelative(..)) => {
                let owner = self.tcx.hir_enclosing_body_owner(ex.hir_id);
                if let Some(to) = self
                    .tcx
                    .typeck(owner)
                    .qpath_res(qpath, ex.hir_id)
                    .opt_def_id()
                {
                    self.record(ex.hir_id, to, false);
                }
            }
            _ => {}
        }
        intravisit::walk_expr(self, ex);
    }
}

/// The cross-crate-stable join key for a def: its `DefPathHash` (hash of the
/// crate's `StableCrateId` + the local def path), rendered hex. Unlike
/// `def_path_str`, this is identical no matter which crate observes `def_id`
/// (`tcx.def_path_hash` reads it from foreign metadata for non-local defs) — so
/// a `RefEdge::to_key` from a *consuming* crate matches the [`ItemFact::key`] in
/// the *defining* crate. This is what makes the Phase-2 reverse index possible.
fn def_key(tcx: TyCtxt<'_>, def_id: DefId) -> String {
    tcx.def_path_hash(def_id).0.to_hex()
}

/// Canonical path for a **local** def, matching [`ItemFact::path`] exactly
/// (`[crate_code, ..def_path_str]`) so a local `to` joins to its item.
fn local_path(tcx: TyCtxt<'_>, crate_code: &str, def_id: DefId) -> Vec<String> {
    let mut path = vec![crate_code.to_string()];
    let rel = tcx.def_path_str(def_id);
    if !rel.is_empty() {
        path.extend(rel.split("::").map(str::to_string));
    }
    path
}

/// Canonical path for a **cross-crate** def: `[crate_code, ..]`. `def_path_str`
/// may or may not already carry the crate segment, so normalize either way.
fn ext_path(tcx: TyCtxt<'_>, def_id: DefId) -> Vec<String> {
    let krate = tcx.crate_name(def_id.krate).to_string().replace('-', "_");
    let rel = tcx.def_path_str(def_id);
    if rel == krate || rel.starts_with(&format!("{krate}::")) {
        rel.split("::").map(str::to_string).collect()
    } else {
        let mut path = vec![krate];
        if !rel.is_empty() {
            path.extend(rel.split("::").map(str::to_string));
        }
        path
    }
}

/// `DefKind` → shared vocabulary string (broader than the item-kind map: a
/// reference can point at fields, variants, params, `use`s, …).
fn def_kind_str(k: DefKind) -> String {
    let s = match k {
        DefKind::Struct => "struct",
        DefKind::Enum => "enum",
        DefKind::Union => "union",
        DefKind::Trait => "trait",
        DefKind::TraitAlias => "trait_alias",
        DefKind::TyAlias => "type",
        DefKind::AssocTy => "assoc_type",
        DefKind::Fn | DefKind::AssocFn => "fn",
        DefKind::Const { .. } | DefKind::AssocConst { .. } => "const",
        DefKind::Static { .. } => "static",
        DefKind::Mod => "mod",
        DefKind::Macro(_) => "macro",
        DefKind::Field => "field",
        DefKind::Variant => "variant",
        DefKind::Ctor(..) => "ctor",
        DefKind::TyParam | DefKind::ConstParam | DefKind::LifetimeParam => "param",
        DefKind::Use => "use",
        _ => "other",
    };
    s.to_string()
}

/// If `def_id` is a **trait-impl** associated item, the stable key of the trait
/// item it implements (`<T as Tr>::f` ⇒ `Tr::f`); `None` for inherent-impl items,
/// trait declarations, and non-assoc defs. `opt_associated_item` is `None` for
/// non-assoc defs; `trait_item_def_id()` is `Some` only for `TraitImpl`. This is
/// the principled inherent-vs-trait-impl signal (see [`ItemFact::trait_item`]).
fn trait_item_key(tcx: TyCtxt<'_>, def_id: DefId) -> Option<String> {
    let ti = tcx.opt_associated_item(def_id)?.trait_item_def_id()?;
    Some(def_key(tcx, ti))
}

/// The parent's `DefKind` in a small closed vocabulary — the principled signal
/// for whether a def is module-level (`mod`), associated (`impl`/`trait`), or
/// body-nested / fn-local (everything else). `None` only for the crate root.
///
/// Why parent kind rather than the item's own: a free `fn`/`const`/`struct`
/// nested in a fn body has the *same* self-`DefKind` as a top-level one — only
/// the parent distinguishes them. And an assoc item's parent (`impl`/`trait`)
/// captures association without depending on `def_path_str`'s rendering.
fn parent_def_kind(tcx: TyCtxt<'_>, def_id: DefId) -> Option<String> {
    let parent = tcx.opt_parent(def_id)?;
    let s = match tcx.def_kind(parent) {
        DefKind::Mod => "mod",
        DefKind::Impl { .. } => "impl",
        DefKind::Trait | DefKind::TraitAlias => "trait",
        DefKind::Fn | DefKind::AssocFn => "fn",
        DefKind::Const { .. } | DefKind::AssocConst { .. } => "const",
        DefKind::Static { .. } => "static",
        DefKind::Closure => "closure",
        _ => "other",
    };
    Some(s.to_string())
}

/// Project a rustc `Span` into a file-relative byte range. `None` for dummy /
/// non-real-file spans.
///
/// Macro-generated spans are projected to their **callsite**
/// (`source_callsite()`) and flagged `from_expansion`: the raw expansion span
/// points into the macro *definition*, a wrong `--fix` write surface, but the
/// callsite is a real user-file position worth keeping for display/identity.
/// See [`Span`] for the policy. Kept verbatim in sync with the driver copy.
/// The export-shaped attributes on a def, from the closed set reachability
/// roots on (`ItemFact::attrs`): an FFI-exported item has no Rust referrer, so
/// these are the only evidence it isn't dead. Name-only — values don't matter.
/// These are *parsed* attributes on this toolchain (`Attribute::Parsed(
/// AttributeKind::…)`), so `has_name` never sees them — `find_attr!` is the
/// prescribed accessor.
fn export_attrs(tcx: TyCtxt<'_>, local_id: LocalDefId) -> Vec<String> {
    use rustc_hir::attrs::AttributeKind;
    use rustc_hir::find_attr;
    let attrs = tcx.hir_attrs(tcx.local_def_id_to_hir_id(local_id));
    let mut out = Vec::new();
    if find_attr!(attrs, AttributeKind::NoMangle(_)) {
        out.push("no_mangle".to_string());
    }
    if find_attr!(attrs, AttributeKind::ExportName { .. }) {
        out.push("export_name".to_string());
    }
    if find_attr!(attrs, AttributeKind::Used { .. }) {
        out.push("used".to_string());
    }
    out
}

fn span_to_ir(sm: &rustc_span::source_map::SourceMap, span: RustcSpan) -> Option<Span> {
    if span.is_dummy() {
        return None;
    }
    let from_expansion = span.from_expansion();
    let span = if from_expansion {
        span.source_callsite()
    } else {
        span
    };
    if span.is_dummy() {
        return None;
    }
    let (lo, hi) = (span.lo(), span.hi());
    let sf = sm.lookup_source_file(lo);
    let file = match &sf.name {
        FileName::Real(rfn) => match rfn.local_path() {
            Some(p) => p.to_string_lossy().into_owned(),
            None => return None,
        },
        _ => return None,
    };
    let start = sf.start_pos.0;
    // 1-based line of `lo`, from the file's own line table (computed on the
    // callsite-projected span, matching `lo`/`hi`). Diagnostic anchors need
    // it, and the extractor is the only place that has the SourceMap.
    let line = sf.lookup_line(sf.relative_position(lo)).map_or(0, |l| l + 1) as u32;
    Some(Span {
        file,
        lo: lo.0.saturating_sub(start),
        hi: hi.0.saturating_sub(start),
        line,
        from_expansion,
    })
}

/// Byte range of a def's **visibility token** (`pub` / `pub(crate)` /
/// `pub(in path)`), or `None` when there is no editable token — the `--fix`
/// tighten write surface. `Node::Item` covers module-level *and* fn-body-nested
/// items; `ImplItem::vis_span()` is `Some` only for inherent-impl items (rustc
/// models trait-impl items as having no independent visibility, so it returns
/// `None`); `ForeignItem` carries a vis token too. Everything else (trait-decl
/// items, the crate root, ctors, …) has none. An **empty** span is rustc's
/// lowering of inherited/private visibility (`shrink_to_lo` at the first token),
/// and an expansion span is a macro-defined token — both are non-surfaces → `None`.
/// Kept verbatim in sync with the driver copy.
fn vis_span_to_ir(
    tcx: TyCtxt<'_>,
    sm: &rustc_span::source_map::SourceMap,
    local_id: LocalDefId,
) -> Option<Span> {
    use rustc_hir::Node;
    let vs = match tcx.hir_node_by_def_id(local_id) {
        Node::Item(it) => it.vis_span,
        Node::ImplItem(ii) => ii.vis_span()?,
        Node::ForeignItem(fi) => fi.vis_span,
        _ => return None,
    };
    if vs.is_empty() || vs.from_expansion() {
        return None;
    }
    span_to_ir(sm, vs)
}

fn write_fragment(fragment: &IrFragment, test_mode: bool) {
    let out_dir = std::env::var("WL_IR_OUT").unwrap_or_else(|_| "target/wl-ir".to_string());
    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        eprintln!("WL-IR: create_dir_all({out_dir}) failed: {e}");
        return;
    }
    let suffix = if test_mode { "+test" } else { "" };
    let path = format!("{out_dir}/{}{suffix}.json", fragment.crate_name);
    // Write-then-rename so a fragment is only ever observed complete: two
    // workspace-lint processes may extract the same workspace concurrently
    // (their compiles serialize on cargo's lock, but a reader in one can
    // otherwise catch the other's half-written JSON). The temp name carries
    // the pid so concurrent writers can't collide on it either.
    let tmp = format!("{path}.{}.tmp", std::process::id());
    match serde_json::to_string_pretty(fragment) {
        Ok(json) => match std::fs::write(&tmp, json).and_then(|()| std::fs::rename(&tmp, &path)) {
            Ok(()) => eprintln!("WL-IR: wrote {} ({} items)", path, fragment.items.len()),
            Err(e) => eprintln!("WL-IR: write({path}) failed: {e}"),
        },
        Err(e) => eprintln!("WL-IR: serialize failed: {e}"),
    }
}
