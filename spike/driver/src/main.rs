//! Step-0 spike driver — a raw `rustc_driver` wrapper.
//!
//! Invoked as a `RUSTC_WORKSPACE_WRAPPER`: cargo calls
//! `wl-driver <path-to-rustc> <rustc args…>`. We run the real compilation via
//! `rustc_driver` and, for the *primary* package only (a workspace member, not a
//! dependency — cargo sets `CARGO_PRIMARY_PACKAGE` for those), hook
//! `after_analysis` to walk `TyCtxt` and emit an [`IrFragment`].
//!
//! Deliberately a raw driver, not a Dylint `LateLintPass`, to de-risk the
//! rustc_private plumbing without `cargo-dylint`/`dylint-link`. The extraction
//! in [`extract`] is written to lift into a `LateLintPass::check_crate`
//! unchanged. See `../README.md` and repo-root `SPIKE-rustc-fidelity-tree.md`.
//!
//! NOTE: `rustc_private` APIs churn per-nightly. Version-sensitive call sites
//! are marked `API:`; expect to adjust them on every toolchain bump (that churn
//
// FROZEN (PR 10): extraction semantics moved on in `extractor/src/lib.rs`
// (ctor/variant→ADT projection, AssocTy items, assoc-type + generic-default
// signature walks, macro-invocation edges). This raw driver still compiles
// (schema kept in sync) but its IR is NO LONGER byte-identical to the
// extractor's — `extractor/` is authoritative; this dies with the spike.
//! is exactly the §9 treadmill signal this spike is meant to measure).
#![feature(rustc_private)]

extern crate rustc_driver;
extern crate rustc_hir;
extern crate rustc_interface;
extern crate rustc_middle;
extern crate rustc_session;
extern crate rustc_span;

use rustc_driver::{Callbacks, Compilation};
use rustc_hir::def::DefKind;
use rustc_hir::def_id::{DefId, LocalDefId, CRATE_DEF_ID, LOCAL_CRATE};
use rustc_hir::intravisit::{self, Visitor};
use rustc_hir::{Expr, ExprKind, HirId, Path, QPath, UsePath};
use rustc_middle::hir::nested_filter;
use rustc_middle::ty::{self, Ty, TyCtxt, TypeSuperVisitable, TypeVisitable, TypeVisitor};
use rustc_span::{FileName, Span as RustcSpan};

use wl_ir::{IrFragment, ItemFact, RefEdge, Span, Visibility};

struct ExtractCallbacks;

impl Callbacks for ExtractCallbacks {
    // API: recent nightlies pass `TyCtxt` directly (the old `Queries` param was
    // removed). If this signature mismatches, the compiler error names the
    // current one.
    fn after_analysis(
        &mut self,
        _compiler: &rustc_interface::interface::Compiler,
        tcx: TyCtxt<'_>,
    ) -> Compilation {
        // A member's build script compiles as its own crate, and cargo names
        // EVERY one `build_script_build` — all of a workspace's build scripts
        // would collide on one fragment filename, and none is a lintable
        // target. Skip them (same guard as the extractor dylib).
        if tcx.crate_name(LOCAL_CRATE).as_str() == "build_script_build" {
            return Compilation::Continue;
        }
        let fragment = extract(tcx);
        write_fragment(&fragment);
        Compilation::Continue
    }
}

/// Walk every local definition and project it into the IR. This is the piece
/// that lifts into a Dylint `LateLintPass::check_crate(cx)` verbatim
/// (`cx.tcx` is the same `TyCtxt`).
fn extract(tcx: TyCtxt<'_>) -> IrFragment {
    let crate_code = tcx.crate_name(LOCAL_CRATE).to_string().replace('-', "_");
    let sm = tcx.sess.source_map();
    let mut items = Vec::new();

    // API: `hir_crate_items(()).definitions()` yields every LocalDefId in the
    // crate.
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
            self_type: self_type_key(tcx, def_id),
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
        // Frozen driver: cargo target-kind detection not backported.
        target_kind: String::new(),
        items,
        references,
    }
}

/// Harvest the crate's reference graph — verbatim from the Dylint `wl-lint`
/// extractor (byte-identity is the point). An HIR walk resolves name-resolved
/// paths, method calls, and type-relative value paths; a second pass adds
/// type-position assoc projections from lowered signatures. Deduped + sorted.
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
    /// Record `enclosing-item-of(at) → to`. `import` marks a `use`/re-export.
    fn record(&mut self, at: HirId, to: DefId, import: bool) {
        let from_id = self.tcx.hir_get_parent_item(at).to_def_id();
        self.record_edge(from_id, to, import, false);
    }

    /// Record `from_id → to`, skipping self-edges (DefId or path-level).
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
        // Drop path-level self-loops (an `impl T` renders by its self-type `T`).
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
            // Frozen driver: `pub use` / glob / alias / use-site-span not
            // backported.
            reexport: false,
            glob: false,
            alias: None,
            span: None,
        });
    }

    /// Second pass: associated-type projections in *type* position
    /// (`<S as Tr>::Item`, `T::Item`, `Self::Out`) — the one reference class the
    /// HIR walk can't resolve (their `PathSegment::res` is `Res::Err`; resolution
    /// is deferred past name-resolution). Instead of driving astconv per HIR node,
    /// walk each item's *lowered* signature (`fn_sig`/`type_of` — cached queries,
    /// no empty-env ICE) and pull the `def_id` of every `Alias(Projection|Inherent)`.
    fn collect_signature_projections(&mut self) {
        for local in self.tcx.hir_crate_items(()).definitions() {
            let did = local.to_def_id();
            let mut proj = ProjVisitor { out: Vec::new() };
            // `skip_binder` (not `instantiate_identity`) keeps the raw ty with its
            // aliases *un-normalized* — we want the projection, not what it resolves
            // to. `fn_sig`/`type_of` are total, cached queries: no empty-env ICE.
            match self.tcx.def_kind(did) {
                DefKind::Fn | DefKind::AssocFn => {
                    self.tcx.fn_sig(did).skip_binder().visit_with(&mut proj);
                }
                DefKind::TyAlias
                | DefKind::Const { .. }
                | DefKind::AssocConst { .. }
                | DefKind::Static { .. } => {
                    self.tcx.type_of(did).skip_binder().visit_with(&mut proj);
                }
                _ => continue,
            }
            for to in proj.out {
                self.record_edge(did, to, false, true);
            }
        }
    }
}

/// Collects the assoc-type `def_id` of every projection in a `ty::Ty` — the
/// `def_id` lives in the `AliasTyKind` variant, not the `AliasTy` itself.
struct ProjVisitor {
    out: Vec<DefId>,
}

impl<'tcx> TypeVisitor<TyCtxt<'tcx>> for ProjVisitor {
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
    type NestedFilter = nested_filter::All;

    fn maybe_tcx(&mut self) -> TyCtxt<'tcx> {
        self.tcx
    }

    fn visit_use(&mut self, path: &'tcx UsePath<'tcx>, hir_id: HirId) {
        // `use`/`pub use`: record each present namespace resolution (`PerNS`) as
        // an **import** edge, not a use-site. We don't call `walk_use`: its
        // default re-drives `visit_path` per namespace, which would re-record
        // these as non-import (their enclosing item is the *module*, so `record`
        // couldn't distinguish them). Kept verbatim in sync with wl-lint.
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
        // Type-dependent resolutions (method calls, type-relative value paths)
        // read the enclosing body's typeck results — path `Res` is empty for them.
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

/// Cross-crate-stable join key for a def: its `DefPathHash` (hex). Identical no
/// matter which crate observes `def_id`, so a consuming crate's `RefEdge::to_key`
/// matches the defining crate's [`ItemFact::key`]. See wl-lint's copy for why
/// `def_path_str` (the `path`) can't serve as the key. Kept verbatim in sync.
fn def_key(tcx: TyCtxt<'_>, def_id: DefId) -> String {
    tcx.def_path_hash(def_id).0.to_hex()
}

/// Canonical path for a **local** def, matching [`ItemFact::path`] exactly.
fn local_path(tcx: TyCtxt<'_>, crate_code: &str, def_id: DefId) -> Vec<String> {
    let mut path = vec![crate_code.to_string()];
    let rel = tcx.def_path_str(def_id);
    if !rel.is_empty() {
        path.extend(rel.split("::").map(str::to_string));
    }
    path
}

/// Canonical path for a **cross-crate** def; normalizes whether or not
/// `def_path_str` already carries the crate segment.
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

/// `DefKind` → shared vocabulary string (broader than the item-kind map).
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

/// Trait item a **trait-impl** assoc item implements (`<T as Tr>::f` ⇒ `Tr::f`),
/// as a stable key; `None` for inherent-impl items, trait decls, non-assoc defs.
/// The inherent-vs-trait-impl signal (see wl-lint's copy). Kept verbatim in sync.
fn trait_item_key(tcx: TyCtxt<'_>, def_id: DefId) -> Option<String> {
    let ti = tcx.opt_associated_item(def_id)?.trait_item_def_id()?;
    Some(def_key(tcx, ti))
}

/// (Kept verbatim in sync with `extractor/src/lib.rs::self_type_key`.)
fn self_type_key(tcx: TyCtxt<'_>, def_id: DefId) -> Option<String> {
    let assoc = tcx.opt_associated_item(def_id)?;
    if assoc.trait_item_def_id().is_some() {
        return None; // trait-impl item: dispatch-judged via `trait_item`
    }
    let parent = tcx.opt_parent(def_id)?;
    if !matches!(tcx.def_kind(parent), DefKind::Impl { .. }) {
        return None; // trait-declaration assoc item
    }
    let self_ty = tcx.type_of(parent).skip_binder();
    let adt = self_ty.ty_adt_def()?;
    Some(def_key(tcx, adt.did()))
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
/// See [`Span`] for the policy. Kept verbatim in sync with the wl-lint copy.
/// The export-shaped attributes on a def (same closed set as the extractor
/// dylib's copy — kept verbatim in sync; parsed-attribute API, see there).
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
        // API: RealFileName::local_path() -> Option<&Path>.
        FileName::Real(rfn) => match rfn.local_path() {
            Some(p) => p.to_string_lossy().into_owned(),
            None => return None,
        },
        _ => return None,
    };
    // Cross-file `hi` (macro splice): no meaningful byte range in `lo`'s file.
    if !sf.contains(hi) {
        return None;
    }
    // 1-based line of `lo` (same emit as the extractor dylib's copy).
    let line = sf
        .lookup_line(sf.relative_position(lo))
        .map_or(0, |l| l + 1) as u32;
    // ON-DISK byte offsets: rustc's positions are in CRLF/BOM-normalized
    // coordinates; consumers slice the raw file. See the extractor copy.
    Some(Span {
        file,
        lo: sf.original_relative_byte_pos(lo).0,
        hi: sf.original_relative_byte_pos(hi).0,
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
/// Kept verbatim in sync with the wl-lint copy.
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

fn write_fragment(fragment: &IrFragment) {
    let out_dir = std::env::var("WL_IR_OUT").unwrap_or_else(|_| "target/wl-ir".to_string());
    if std::fs::create_dir_all(&out_dir).is_err() {
        return;
    }
    let path = format!("{out_dir}/{}.json", fragment.crate_name);
    // Write-then-rename so a fragment is only ever observed complete (same
    // torn-read guard as the extractor dylib's copy).
    let tmp = format!("{path}.{}.tmp", std::process::id());
    if let Ok(json) = serde_json::to_string_pretty(fragment) {
        let _ = std::fs::write(&tmp, json).and_then(|()| std::fs::rename(&tmp, &path));
    }
}

fn main() -> std::process::ExitCode {
    // As RUSTC_WORKSPACE_WRAPPER, argv = [wl-driver, <rustc>, <args…>].
    // Drop our own program name; rustc_driver wants args[0] = the compiler.
    let mut args: Vec<String> = std::env::args().collect();
    args.remove(0);

    let primary = std::env::var_os("CARGO_PRIMARY_PACKAGE").is_some();

    // API: `run_compiler(&args, &mut callbacks)`; wrap in catch_with_exit_code
    // for the correct process exit on rustc errors (returns an ExitCode).
    rustc_driver::catch_with_exit_code(|| {
        if primary {
            rustc_driver::run_compiler(&args, &mut ExtractCallbacks);
        } else {
            rustc_driver::run_compiler(&args, &mut NoopCallbacks);
        }
    })
}

struct NoopCallbacks;
impl Callbacks for NoopCallbacks {}
