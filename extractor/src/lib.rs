//! Phase-1 rustc-fidelity IR extractor: a Dylint `LateLintPass` that walks the
//! `TyCtxt` of the crate under compilation and emits a byte-precise
//! [`IrFragment`] (SPIKE-rustc-fidelity-tree.md §4/§11).
//!
//! This "lint" never warns: it harvests facts into `$WL_IR_OUT/<crate>.wlir`
//! (the rkyv IR channel). Real diagnostics (the findings channel) ride Dylint's
//! native lint path separately; the two never mix (SPIKE §4). The
//! findings-channel round-trip itself was proven by the retired pivot spike
//! (WS1-A4, SPIKE §12.6/§12b) with a demo lint that did not graduate with
//! this package; the spike's raw `rustc_driver` twin also proved this walk
//! is host-agnostic before it retired.
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
use rustc_hir::{
    Expr, ExprKind, HirId, Item, ItemKind, Node, Pat, PatKind, Path, QPath, UseKind, UsePath,
};
use rustc_lint::{LateContext, LateLintPass, LintStore};
use rustc_middle::hir::nested_filter;
use rustc_middle::ty::{self, Ty, TyCtxt, TypeSuperVisitable, TypeVisitable, TypeVisitor};
use rustc_session::Session;
use rustc_span::hygiene::ExpnKind;
use rustc_span::{FileName, Span as RustcSpan};

use wl_ir::{IrFragment, ItemFact, RefEdge, Span, Visibility};

// The dylib entry glue `declare_late_lint!` used to supply: the `dylint_version`
// export + `extern crate rustc_driver`. Required for the dylib to load.
dylint_linting::dylint_library!();

rustc_session::declare_lint! {
    /// ### What it does
    ///
    /// Walks `TyCtxt` for the crate under compilation and writes a byte-precise
    /// IR fragment to `$WL_IR_OUT/<crate>.wlir`. Phase 1 of the rustc-fidelity
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
        let tcx = cx.tcx;
        // A member's build script compiles as its own crate, and cargo names
        // EVERY one `build_script_build` — so the fragment is keyed on the
        // owning package (`CARGO_PKG_NAME`, set on every unit of the package)
        // instead. References-only: build.rs edges are what make a crate
        // consumed from build scripts count as used (unused-pub), while its
        // defs are nothing any lint judges — emitting them would insert a
        // phantom `build_script_build` member into the assembly's crate set.
        if tcx.crate_name(LOCAL_CRATE).as_str() == "build_script_build" {
            let Ok(pkg) = std::env::var("CARGO_PKG_NAME") else {
                // No owning package identified — never write a colliding name.
                return;
            };
            let mut fragment = extract(tcx);
            fragment.target_kind = "build".to_string();
            fragment.items = Vec::new();
            write_fragment(&fragment, &format!("{}@build", pkg.replace('-', "_")));
            return;
        }
        // A member consumed by a *run-requiring host unit* (another member's
        // build script, or a proc-macro member) compiles a SECOND time under
        // `cargo check`: a Build-mode rlib (`--emit=link`) alongside its
        // primary metadata-only Check unit. Cargo hashes the compile mode
        // into `-Cmetadata`, so the two copies carry different
        // `StableCrateId`s — different `DefPathHash` generations. Letting the
        // Build-mode copy write would race the Check-mode fragment on one
        // filename, and when it wins, every Check-mode consumer edge misses
        // the def keys (verified live: a member declared in both
        // [dependencies] and [build-dependencies] read as unused). Skip it:
        // the exemptions keep every wanted link-emitting unit — proc-macros
        // (their primary shape), `--test` harnesses, bins, and integration
        // tests/benches (`CARGO_TARGET_TMPDIR`).
        let emits_link = tcx
            .sess
            .opts
            .output_types
            .contains_key(&rustc_session::config::OutputType::Exe);
        let is_proc_macro = tcx
            .crate_types()
            .contains(&rustc_session::config::CrateType::ProcMacro);
        if emits_link
            && !is_proc_macro
            && !tcx.sess.opts.test
            && std::env::var_os("CARGO_BIN_NAME").is_none()
            && std::env::var_os("CARGO_TARGET_TMPDIR").is_none()
        {
            return;
        }
        let fragment = extract(tcx);
        // `--test` mode compiles a distinct crate variant (cfg(test) on, #[test]
        // fns kept for the harness). It can coexist with the plain-lib build of
        // the same crate, which would race on one filename — so key the output on
        // it. Lets the fidelity oracle compare config-matched IRs (SPIKE §7/§10).
        // Bins get an infix for the same reason (a package's bin may share the
        // lib's crate name — see `IrFragment::target_kind`).
        let kind_infix = if fragment.target_kind == "bin" {
            "@bin"
        } else {
            ""
        };
        // `+test` marks every test-config unit, not just `--test` harnesses: a
        // `[[test]] harness = false` target compiles WITHOUT `--test` (cargo
        // passes the flag only for libtest harnesses), but it is still a test
        // unit the completeness guard expects under `--tests` as
        // `<name>+test.wlir` — keying on `opts.test` alone made such workspaces
        // fail the guard forever. `target_kind == "test"` (CARGO_TARGET_TMPDIR)
        // covers the harness-less shape; there is no filename collision to
        // disambiguate for them (an integration test compiles exactly once),
        // the suffix is purely the guard's naming contract.
        let suffix = if tcx.sess.opts.test || fragment.target_kind == "test" {
            "+test"
        } else {
            ""
        };
        let stem = format!("{}{kind_infix}{suffix}", fragment.crate_name);
        write_fragment(&fragment, &stem);
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
            // Assoc types too: a trait-impl `type Out = Resp;` is the E0446
            // shape — the binding is a *signature position* for `Resp`, and
            // `exposed_in_public_signature` needs the assoc item as the
            // edge's (public) `from` def.
            DefKind::TyAlias | DefKind::AssocTy => "type",
            DefKind::Fn | DefKind::AssocFn => "fn",
            DefKind::Const { .. } | DefKind::AssocConst { .. } => "const",
            DefKind::Static { .. } => "static",
            DefKind::Mod => "mod",
            DefKind::Macro(_) => "macro",
            _ => continue, // closures, impls, params, ctors, … — not tree items
        };
        items.push(item_fact(tcx, sm, &crate_code, local_id, kind));

        // Fields are not tree items for the verdict, but the dead-field
        // narrow guard needs them as edge landing points: narrowing a type
        // un-exempts its never-READ fields from rustc `dead_code`. They are
        // not in `definitions()` — enumerate them off the ADT.
        if matches!(kind, "struct" | "enum" | "union") {
            for f in tcx.adt_def(def_id).all_fields() {
                if let Some(flocal) = f.did.as_local() {
                    items.push(item_fact(tcx, sm, &crate_code, flocal, "field"));
                }
            }
        }
    }

    let references = collect_references(tcx, &crate_code);
    let loaded_files = collect_loaded_files(sm);

    IrFragment {
        schema_version: wl_ir::SCHEMA_VERSION,
        crate_name: crate_code,
        target_kind: target_kind(tcx).to_string(),
        // Whether this unit compiled with `cfg(test)` (`--test`). Carried
        // in-archive so the assembler can classify the unit's edges as test
        // reach (`IrFragment::is_test_cfg` docs the split). NOT identical to
        // the `+test` filename suffix: a `harness = false` integration test
        // gets the suffix (it's a test unit) but compiles without `--test`,
        // so `is_test_cfg` is false — classification must also honor
        // `target_kind == "test"`.
        is_test_cfg: tcx.sess.opts.test,
        items,
        references,
        loaded_files,
    }
}

/// Every source file rustc opened for this compilation unit, restricted to the
/// crate's own package directory.
///
/// This is the ground truth behind `orphan-file`. The `SourceMap` holds a
/// `SourceFile` for each file that was *parsed as source* — which is exactly
/// the set we want: it includes files reached through `#[cfg_attr(…, path)]`,
/// `macro_rules!`-generated `mod`s, and `include!` in any position, because by
/// the time a `LateLintPass` runs, expansion has already happened.
///
/// `include_str!` / `include_bytes!` targets land here too — rustc registers
/// them in the `SourceMap` so diagnostics can point into them (verified against
/// the pinned nightly, not assumed). The fast tier's `declared_reach` names
/// them independently, so the day that stops being true a live file degrades to
/// a harmless coverage-gap finding rather than a "delete this file" claim.
///
/// The `CARGO_MANIFEST_DIR` filter drops registry dependencies and `OUT_DIR`
/// generated code. Paths are canonicalized on both sides of the comparison:
/// rustc hands us paths relative to its working directory (the workspace root),
/// and on macOS `/tmp` vs `/private/tmp` would otherwise never match.
fn collect_loaded_files(sm: &rustc_span::source_map::SourceMap) -> Vec<String> {
    let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") else {
        return Vec::new();
    };
    let manifest_dir = std::path::PathBuf::from(manifest_dir);
    let manifest_dir = manifest_dir.canonicalize().unwrap_or(manifest_dir);
    let cwd = std::env::current_dir().ok();

    let mut out = Vec::new();
    for source_file in sm.files().iter() {
        let FileName::Real(rfn) = &source_file.name else {
            continue;
        };
        let Some(path) = rfn.local_path() else {
            continue;
        };
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            match &cwd {
                Some(dir) => dir.join(path),
                None => continue,
            }
        };
        let absolute = absolute.canonicalize().unwrap_or(absolute);
        if absolute.starts_with(&manifest_dir) {
            out.push(absolute.to_string_lossy().into_owned());
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Project one local def into its [`ItemFact`] — the whole emit policy for a
/// definition, shared by the `definitions()` walk and the per-ADT field
/// enumeration (fields have no `definitions()` entry).
fn item_fact(
    tcx: TyCtxt<'_>,
    sm: &rustc_span::source_map::SourceMap,
    crate_code: &str,
    local_id: LocalDefId,
    kind: &str,
) -> ItemFact {
    let def_id = local_id.to_def_id();
    // def_path_str omits the crate for local items; prepend the code name.
    let mut path = vec![crate_code.to_string()];
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

    ItemFact {
        path,
        key: def_key(tcx, def_id),
        kind: kind.to_string(),
        parent_kind: parent_def_kind(tcx, def_id),
        trait_item: trait_item_key(tcx, def_id),
        self_type: self_type_key(tcx, def_id),
        visibility,
        span: span_to_ir(sm, tcx.def_span(def_id)),
        full_span: full_item_span(tcx, sm, local_id),
        vis_span: vis_span_to_ir(tcx, sm, local_id),
        attrs: export_attrs(tcx, local_id),
        self_kind: assoc_self_kind(tcx, local_id),
        self_copy: assoc_self_copy(tcx, def_id),
    }
}

/// Which cargo target this compilation unit is. `rustc` alone can't tell: under
/// `--test` every unit is an executable (`crate_types() == [Executable]`), so a
/// bin's unit-test harness and the lib's are indistinguishable from the session
/// options. Cargo's per-unit environment is the discriminator (verified
/// empirically on a lib+bin+integration-test package, both configs):
/// `CARGO_BIN_NAME` is set exactly for bin units — *including* their `--test`
/// harnesses — and unset for lib, proc-macro, and integration-test units;
/// `CARGO_TARGET_TMPDIR` is set only for integration tests and benches.
fn target_kind(tcx: TyCtxt<'_>) -> &'static str {
    if std::env::var_os("CARGO_BIN_NAME").is_some() {
        return "bin";
    }
    if std::env::var_os("CARGO_TARGET_TMPDIR").is_some() {
        return "test";
    }
    if tcx
        .crate_types()
        .contains(&rustc_session::config::CrateType::ProcMacro)
    {
        return "proc-macro";
    }
    "lib"
}

/// Harvest the crate's reference graph by walking the whole HIR and resolving
/// every name-resolved path to the def it points at, attributed to the nearest
/// enclosing item (`from`). Deduped + sorted so the fragment is deterministic.
///
/// Coverage: name-resolved paths (`Res`-carrying — function/type/trait/ADT
/// references, imports), method calls `x.f()`, and type-relative **value** paths
/// `Type::assoc_fn` / `Type::CONST` (via the enclosing body's `typeck`). A second
/// pass ([`RefCollector::collect_signature_projections`]) then adds **type-position
/// assoc projections** (`<T as Trait>::Item`) and the whole privacy-checked
/// signature surface — types, generic-parameter defaults, bounds and
/// `where`-clauses, supertraits, assoc-type/`impl Trait` item bounds, `dyn`
/// principals, field types — from the lowered signatures and the written
/// predicate queries.
fn collect_references(tcx: TyCtxt<'_>, crate_code: &str) -> Vec<RefEdge> {
    let mut collector = RefCollector {
        tcx,
        crate_code,
        edges: Vec::new(),
        write_fields: std::collections::HashSet::new(),
    };
    tcx.hir_walk_toplevel_module(&mut collector);
    collector.collect_signature_projections();
    collector.collect_trait_scope_imports();
    let mut edges = collector.edges;
    // Dedup on edge *identity* (everything but the span), keeping the first
    // (lowest) span: five calls to the same def are one edge, anchored at the
    // earliest use-site. `span` is `RefEdge`'s last field, so the derived
    // full-struct sort already groups identical identities span-ascending —
    // deterministic across runs (byte-identity requirement).
    edges.sort();
    edges.dedup_by(|later, first| edge_identity(later) == edge_identity(first));
    edges
}

/// An edge's identity — every field except the use-site span.
#[allow(clippy::type_complexity)]
fn edge_identity(
    e: &RefEdge,
) -> (
    &[String],
    &[String],
    &str,
    &str,
    &str,
    [bool; 8],
    Option<&str>,
    Option<&str>,
) {
    (
        &e.from,
        &e.to,
        &e.from_key,
        &e.to_key,
        &e.to_kind,
        [
            e.external,
            e.import,
            e.in_signature,
            e.reexport,
            e.glob,
            // A written `Type::method` path and a `.method()` call to the same
            // def must NOT merge: only the written form credits the type's
            // import, and the global dedup keeps just one representative.
            e.receiver_resolved,
            // A trait-scope fact and a written path to the same target are
            // different evidence classes — the glob accounting reads them
            // through different rules, so they must both survive dedup.
            e.trait_scope,
            e.extern_root,
        ],
        e.alias.as_deref(),
        e.via.as_deref(),
    )
}

/// The classification of one reference edge — how the name was reached, not
/// where. `default()` is a plain body/path use-site.
#[derive(Default, Clone)]
struct EdgeFlags {
    import: bool,
    in_signature: bool,
    reexport: bool,
    glob: bool,
    /// Typeck receiver-based resolution (method call / field read) — no
    /// written path, so no `use` statement was involved. See
    /// [`RefEdge::receiver_resolved`].
    receiver_resolved: bool,
    /// Single-name imports only: the local binding name (`use a::B as C` ⇒ `C`).
    alias: Option<String>,
    /// Glob imports only: the resolver's glob_map for this `use` item — see
    /// [`RefEdge::glob_used_names`].
    glob_used_names: Vec<String>,
    /// A typeck `used_trait_imports` fact, not a written path — see
    /// [`RefEdge::trait_scope`].
    trait_scope: bool,
    /// The written path's first segment is an extern crate root — see
    /// [`RefEdge::extern_root`].
    extern_root: bool,
    /// The extern crate the written path routes through when it isn't the
    /// defining crate (`use shim::Item` resolving through a re-export) — see
    /// [`RefEdge::via`]. Computed by [`RefCollector::via_crate`].
    via: Option<String>,
    /// Single-name import leaves only: the enclosing `use …;` declaration span
    /// and this leaf's own written span — see [`RefEdge::decl_span`] /
    /// [`RefEdge::elem_span`]. `None` on every non-import and on glob/list-stem
    /// `use` nodes (no single deletable leaf).
    decl_span: Option<RustcSpan>,
    elem_span: Option<RustcSpan>,
}

struct RefCollector<'a, 'tcx> {
    tcx: TyCtxt<'tcx>,
    crate_code: &'a str,
    edges: Vec<RefEdge>,
    /// Field exprs in plain-assignment LHS position (`x.f = v`): writes, not
    /// reads. rustc `dead_code` flags a field with writes but no reads, so
    /// the read-edge collection must not count them (compound `+=` reads).
    /// Parents visit before children, so the `Assign` arm marks the LHS
    /// before the `Field` arm sees it.
    write_fields: std::collections::HashSet<HirId>,
}

impl<'a, 'tcx> RefCollector<'a, 'tcx> {
    /// Invoking a macro is *using* it, but the invocation itself leaves no
    /// HIR node — only nodes generated by the expansion, whose spans carry
    /// the expansion context. Record one edge per generated node to the
    /// invoked macro's def (the global dedup collapses them): the by-name
    /// usage syn counted textually, recovered from `ExpnData`. Covers bang
    /// macros, derives, and attribute macros alike (a derive expansion also
    /// credits its proc-macro crate for unused-deps).
    ///
    /// The **whole expansion chain** is walked, not just the innermost
    /// expansion: `a!` expanding solely to `b!` must credit `a` too — the
    /// outer macro is what the source names (and what its glob import
    /// supplies), while only `b` appears in the innermost `ExpnData`.
    fn record_macro_expansion(&mut self, span: RustcSpan, at: HirId) {
        let mut ctxt = span.ctxt();
        while !ctxt.is_root() {
            let data = ctxt.outer_expn_data();
            if let ExpnKind::Macro(_, _) = data.kind
                && let Some(mac) = data.macro_def_id
            {
                self.record(at, mac, span);
            }
            let parent = data.call_site.ctxt();
            if parent == ctxt {
                break; // defensive: never spin on a self-referential context
            }
            ctxt = parent;
        }
    }

    /// Record `enclosing-item-of(at) → to` as a use-site edge anchored at `span`.
    fn record(&mut self, at: HirId, to: DefId, span: RustcSpan) {
        let from_id = self.tcx.hir_get_parent_item(at).to_def_id();
        self.record_edge(from_id, to, Some(span), EdgeFlags::default());
    }

    /// [`record`](Self::record), for typeck receiver-based resolutions —
    /// method calls and field reads, which involve no written path (see
    /// [`RefEdge::receiver_resolved`]). Pattern-field reads count too: the
    /// pattern's *type* path (`S { f, .. }`) emits its own path edge, so the
    /// field edge itself carries no import-crediting information.
    fn record_receiver(&mut self, at: HirId, to: DefId, span: RustcSpan) {
        let from_id = self.tcx.hir_get_parent_item(at).to_def_id();
        let flags = EdgeFlags {
            receiver_resolved: true,
            ..EdgeFlags::default()
        };
        self.record_edge(from_id, to, Some(span), flags);
    }

    /// Record an import (`use`) edge with its declaration-shape metadata
    /// (`reexport` iff the declaration is `pub`, `glob`/`alias` per `UseKind`).
    fn record_import(&mut self, at: HirId, to: DefId, span: RustcSpan, flags: EdgeFlags) {
        let from_id = self.tcx.hir_get_parent_item(at).to_def_id();
        self.record_edge(from_id, to, Some(span), flags);
    }

    /// Record `from_id → to`, skipping self-edges. Two flavours of self-edge:
    /// same-`DefId` (an item's own def in its signature/body — noise, not usage),
    /// and path-level (`def_path_str` renders an `impl T` block by its self-type,
    /// so a ref to `T` from inside its own impl collapses to from==to).
    /// `flags.in_signature` marks edges from the lowered-signature pass (the HIR
    /// walk may emit the same reference unflagged; consumers of the flag are
    /// boolean, so the near-duplicate is harmless and keeps both provenances).
    fn record_edge(
        &mut self,
        from_id: DefId,
        to: DefId,
        span: Option<RustcSpan>,
        flags: EdgeFlags,
    ) {
        // A path to a constructor or enum variant is a use of the owning ADT:
        // `let _ = Unit;` resolves to the unit-struct CTOR and `Status::Ok`
        // to the variant — neither is a tree item, so an unprojected edge
        // would be dropped at assembly and an ADT used only through
        // construction/variants would read unused.
        let to = projected_target(self.tcx, to);
        if from_id == to {
            return;
        }
        let from = local_path(self.tcx, self.crate_code, from_id);
        // The lexical enclosing module — NOT derivable from `from`:
        // `def_path_str` renders impl members at their self-type's path
        // (`Type::method`, `<Type as Trait>::method`), hiding the module the
        // impl block is written in. Import-scope resolution downstream needs
        // the lexical module (rustc resolves a body's names through the
        // imports of the module the code sits in). Module def paths are
        // bracket-free, so `local_path` splits them losslessly.
        let from_module = from_id
            .as_local()
            .map(|l| {
                local_path(
                    self.tcx,
                    self.crate_code,
                    self.tcx.parent_module_from_def_id(l).to_def_id(),
                )
            })
            .unwrap_or_default();
        let external = to.krate != LOCAL_CRATE;
        let to_path = if external {
            ext_path(self.tcx, to)
        } else {
            local_path(self.tcx, self.crate_code, to)
        };
        if from == to_path {
            return;
        }
        let sm = self.tcx.sess.source_map();
        self.edges.push(RefEdge {
            from,
            from_module,
            to: to_path,
            from_key: def_key(self.tcx, from_id),
            to_key: def_key(self.tcx, to),
            to_kind: def_kind_str(self.tcx.def_kind(to)),
            external,
            import: flags.import,
            in_signature: flags.in_signature,
            receiver_resolved: flags.receiver_resolved,
            reexport: flags.reexport,
            glob: flags.glob,
            alias: flags.alias,
            glob_used_names: flags.glob_used_names,
            trait_scope: flags.trait_scope,
            extern_root: flags.extern_root,
            via: flags.via,
            span: span.and_then(|s| span_to_ir(sm, s)),
            decl_span: flags.decl_span.and_then(|s| span_to_ir(sm, s)),
            elem_span: flags.elem_span.and_then(|s| span_to_ir(sm, s)),
        });
    }

    /// The extern crate the written path routes through: the first path
    /// segment's *own* resolution, when it's another crate's root and that
    /// crate differs from the one defining the final target. This is the
    /// `use web_time::Instant` case — the whole-path res follows the shim's
    /// `pub use std::time::*` to the def in `std`, and this segment is the
    /// only record of which dependency the source actually names. `None` for
    /// local roots (`crate`/`self`/`super`/module paths) and for direct uses
    /// where the written root already is the defining crate.
    fn via_crate(&self, segments: &[rustc_hir::PathSegment<'tcx>], to: DefId) -> Option<String> {
        let seg_did = segments.first()?.res.opt_def_id()?;
        if !seg_did.is_crate_root() || seg_did.krate == LOCAL_CRATE || seg_did.krate == to.krate {
            return None;
        }
        Some(
            self.tcx
                .crate_name(seg_did.krate)
                .as_str()
                .replace('-', "_"),
        )
    }

    /// Is the written path's first segment an extern crate root? Such a
    /// resolution bypassed every local `use` — see [`RefEdge::extern_root`].
    /// Unlike [`via_crate`](Self::via_crate), set even when the written root
    /// IS the defining crate.
    ///
    /// A `$crate`-rooted path is the same fact by construction — it is *the*
    /// mechanism a macro names its own crate, resolved at the macro's
    /// definition site — and must be caught by ident, because per-segment
    /// `res` is rarely populated on body paths (`tracing::debug!` expands to
    /// `$crate::…::Event::dispatch`, whose root segment carries no res; the
    /// 2026-07-08 LeaveDates finding).
    fn extern_root(&self, segments: &[rustc_hir::PathSegment<'tcx>]) -> bool {
        segments.first().is_some_and(|s| {
            s.ident.name == rustc_span::symbol::kw::DollarCrate
                || s.res
                    .opt_def_id()
                    .is_some_and(|d| d.is_crate_root() && d.krate != LOCAL_CRATE)
        })
    }

    /// Typeck's `used_trait_imports` harvest: for every typeck root, the
    /// `use` items whose target had to be in scope for some method call in
    /// its body to resolve. Emitted as [`RefEdge::trait_scope`] facts (from
    /// the body owner, to the `use` item's own resolution target — the trait
    /// for a single import, the module for a glob). This is the only record
    /// of a glob kept alive purely by trait-method syntax: the call itself is
    /// receiver-resolved (no written path) and the glob_map only tracks
    /// name-resolutions.
    fn collect_trait_scope_imports(&mut self) {
        for owner in self.tcx.hir_body_owners() {
            // Closures and other nested bodies share their root's tables;
            // query only the roots (each table is read once).
            if self.tcx.typeck_root_def_id(owner.to_def_id()) != owner.to_def_id() {
                continue;
            }
            let typeck = self.tcx.typeck(owner);
            // `UnordSet` only yields its contents in a stable order; any
            // order would do (the final edge sort+dedup normalizes), but the
            // stable one is free of caveats.
            let use_ids: Vec<LocalDefId> = self.tcx.with_stable_hashing_context(|mut hcx| {
                typeck
                    .used_trait_imports
                    .items()
                    .copied()
                    .into_sorted(&mut hcx)
            });
            for use_id in use_ids {
                let Node::Item(item) = self.tcx.hir_node_by_def_id(use_id) else {
                    continue;
                };
                let ItemKind::Use(path, _) = item.kind else {
                    continue;
                };
                for res in path.res.present_items() {
                    if let Some(to) = res.opt_def_id() {
                        self.record_edge(
                            owner.to_def_id(),
                            to,
                            Some(item.span),
                            EdgeFlags {
                                trait_scope: true,
                                ..EdgeFlags::default()
                            },
                        );
                    }
                }
            }
        }
    }

    /// Second pass over each item's *lowered* signature (`fn_sig`/`type_of`/
    /// predicate queries — all cached, no empty-env ICE), with two jobs:
    ///
    /// 1. Associated-type projections in *type* position (`<S as Tr>::Item`,
    ///    `T::Item`, `Self::Out`) — the one reference class the HIR walk can't
    ///    resolve (their `PathSegment::res` is `Res::Err`; resolution is
    ///    deferred past name-resolution).
    /// 2. Every def *named* in the signature, flagged
    ///    [`RefEdge::in_signature`] — the substrate of
    ///    `exposed_in_public_signature` (a def named in a pub signature must
    ///    not be visibility-tightened: E0445/E0446 / `private_interfaces` /
    ///    `private_bounds`). "Signature" here is the whole privacy-checked
    ///    surface: parameter/return/field types, generic-parameter defaults,
    ///    inline bounds and `where`-clauses (`explicit_predicates_of`),
    ///    supertraits (`explicit_super_predicates_of`), trait-decl assoc-type
    ///    bounds and `impl Trait` bounds (`explicit_item_bounds`), and `dyn`
    ///    principals — the *explicit* (written) queries, matching what rustc's
    ///    privacy pass judges; elaboration would add phantom supertrait
    ///    closure the source never names.
    fn collect_signature_projections(&mut self) {
        for local in self.tcx.hir_crate_items(()).definitions() {
            let did = local.to_def_id();
            let kind = self.tcx.def_kind(did);
            let mut sig = SigVisitor::default();
            // `skip_binder` (not `instantiate_identity`) keeps aliases *un-normalized*
            // — we want the projection, not what it resolves to.
            match kind {
                DefKind::Fn | DefKind::AssocFn => {
                    self.tcx.fn_sig(did).skip_binder().visit_with(&mut sig);
                }
                DefKind::TyAlias
                | DefKind::Const { .. }
                | DefKind::AssocConst { .. }
                | DefKind::Static { .. } => {
                    self.tcx.type_of(did).skip_binder().visit_with(&mut sig);
                }
                // A trait-impl `type Out = Resp;` binding IS a signature
                // position for `Resp` (E0446 if tightened). Only impl-side
                // assoc types have a `type_of` — a defaultless trait-decl one
                // would panic the query; ITS surface is the written bounds
                // (`type Item: Bound`), i.e. its item bounds.
                DefKind::AssocTy => {
                    if matches!(
                        self.tcx.def_kind(self.tcx.parent(did)),
                        DefKind::Impl { .. }
                    ) {
                        self.tcx.type_of(did).skip_binder().visit_with(&mut sig);
                    } else {
                        sig.collect_clauses(self.tcx.explicit_item_bounds(did).skip_binder());
                    }
                }
                // ADTs carry no lowered signature of their own — their field
                // types are handled below with the FIELD as the edge's `from`,
                // and their bounds/defaults through the common tail.
                DefKind::Struct | DefKind::Enum | DefKind::Union => {}
                // A trait's own signature surface is its supertrait list
                // (`pub trait A: B` — narrowing `B` is E0445); where-clauses
                // ride the common predicate sweep below.
                DefKind::Trait | DefKind::TraitAlias => {
                    sig.collect_clauses(self.tcx.explicit_super_predicates_of(did).skip_binder());
                }
                _ => continue,
            }
            // Generic-parameter defaults are signature positions too:
            // `pub struct Wrapper<T = DefaultArg>` exposes `DefaultArg`
            // exactly like a field type would.
            for param in &self.tcx.generics_of(did).own_params {
                if let ty::GenericParamDefKind::Type {
                    has_default: true, ..
                } = param.kind
                {
                    self.tcx
                        .type_of(param.def_id)
                        .skip_binder()
                        .visit_with(&mut sig);
                }
            }
            // The written predicates: inline generic bounds (`<R: Bound>` —
            // the `coalesce<R: ByteRange>` blind spot), `where`-clauses, and
            // argument-position `impl Trait` (a synthetic param whose bound
            // lands here). `fn_sig`/`type_of` carry only the param *itself*.
            sig.collect_clauses(self.tcx.explicit_predicates_of(did).predicates);
            self.flush_signature_edges(did, sig);

            // Field types: fields are not in `definitions()` and narrowing a
            // pub field's type is the same E0446 as a return type. The edge's
            // `from` is the FIELD def, so the stable side can gate exposure on
            // the field's own visibility (a private field's type stays
            // legitimately tightenable).
            if matches!(kind, DefKind::Struct | DefKind::Enum | DefKind::Union) {
                for f in self.tcx.adt_def(did).all_fields() {
                    if f.did.is_local() {
                        let mut fsig = SigVisitor::default();
                        self.tcx.type_of(f.did).skip_binder().visit_with(&mut fsig);
                        self.flush_signature_edges(f.did, fsig);
                    }
                }
            }
        }
    }

    /// Drain a [`SigVisitor`] into `in_signature` edges from `from`. Opaques
    /// (`impl Trait`) resolve through their *item bounds* — which may surface
    /// further opaques (`impl Iterator<Item = impl …>`), hence the worklist.
    /// No use-site span: these come from the *lowered* signature, where a
    /// projection may have no single surface token at all.
    fn flush_signature_edges(&mut self, from: DefId, mut sig: SigVisitor) {
        let mut seen = std::collections::HashSet::new();
        while let Some(opaque) = sig.opaques.pop() {
            // Local only: a foreign opaque leaking in through an alias has no
            // extractable fragment here anyway (its defining crate emits it).
            if seen.insert(opaque) && opaque.is_local() {
                sig.collect_clauses(self.tcx.explicit_item_bounds(opaque).skip_binder());
            }
        }
        for to in sig.out {
            self.record_edge(
                from,
                to,
                None,
                EdgeFlags {
                    in_signature: true,
                    ..EdgeFlags::default()
                },
            );
        }
    }
}

/// Collects the `def_id` of every def *named* in a `ty::Ty` tree: assoc-type
/// projections (whose `def_id` lives in the `AliasTyKind` variant), plain
/// ADTs, and `dyn`-type traits. All feed the same edge stream; the named defs
/// additionally carry the signature-position flag's meaning for
/// `exposed_in_public_signature`. Opaques (`impl Trait`) are queued instead:
/// their named surface is their *item bounds*, resolved by the caller
/// (`flush_signature_edges` — this visitor has no `TyCtxt`).
#[derive(Default)]
struct SigVisitor {
    out: Vec<DefId>,
    opaques: Vec<DefId>,
}

impl SigVisitor {
    /// Sweep a written clause list (`explicit_predicates_of` /
    /// `explicit_super_predicates_of` / `explicit_item_bounds`): the bound
    /// TRAIT itself is a def named by the signature but is not a `Ty`, so the
    /// type walk alone can't collect it — pull it off the clause kind, then
    /// visit the clause for every embedded type (bound arguments like
    /// `R: AsRef<Named>`, outlives'd types, const-arg types).
    fn collect_clauses<'tcx>(&mut self, clauses: &[(ty::Clause<'tcx>, RustcSpan)]) {
        for (clause, _) in clauses {
            match clause.kind().skip_binder() {
                ty::ClauseKind::Trait(tp) => self.out.push(tp.trait_ref.def_id),
                ty::ClauseKind::Projection(pp) => self.out.push(pp.projection_term.def_id),
                _ => {}
            }
            clause.visit_with(self);
        }
    }
}

impl<'tcx> TypeVisitor<TyCtxt<'tcx>> for SigVisitor {
    fn visit_ty(&mut self, t: Ty<'tcx>) {
        match t.kind() {
            ty::Alias(alias) => match alias.kind {
                ty::AliasTyKind::Projection { def_id } | ty::AliasTyKind::Inherent { def_id } => {
                    self.out.push(def_id);
                }
                ty::AliasTyKind::Opaque { def_id } => self.opaques.push(def_id),
                _ => {}
            },
            ty::Adt(adt, _) => self.out.push(adt.did()),
            // `dyn Trait` names its traits without any `Ty` node for them.
            ty::Dynamic(preds, ..) => {
                for ep in preds.iter() {
                    match ep.skip_binder() {
                        ty::ExistentialPredicate::Trait(tr) => self.out.push(tr.def_id),
                        ty::ExistentialPredicate::Projection(p) => self.out.push(p.def_id),
                        ty::ExistentialPredicate::AutoTrait(d) => self.out.push(d),
                    }
                }
            }
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
        //
        // The use declaration's own visibility distinguishes a re-export
        // (`pub use` — the target must stay `pub`: E0364/E0365) from a plain
        // same-crate import (see `RefEdge::reexport`).
        let reexport = matches!(
            self.tcx.visibility(hir_id.owner.def_id.to_def_id()),
            ty::Visibility::Public
        );
        // A glob (`use m::*`) resolves to the module def exactly like a plain
        // `use a::m` does — only the owning item's `UseKind` tells them apart,
        // and the architecture lint judges them differently (a glob imports the
        // module's *contents*, tested as a representative child). A single-name
        // use carries its local binding ident — the only record of a
        // `use a::B as C` rename.
        //
        // For a `Single` leaf we also capture the two `--fix` deletion surfaces
        // (empirically pinned by `probe.rs` §18 — rustc's use-tree lowering is
        // not obvious here):
        //
        //  - `decl_span` = the leaf item's own span. For a **standalone**
        //    `use a::b;` this is the whole statement (the delete surface); for a
        //    **brace-list** leaf rustc collapses it to just the leaf, so it
        //    equals `elem_span`. That equality IS the brace discriminator the
        //    lint uses: `decl_span ⊋ elem_span` ⇒ whole-statement delete;
        //    `decl_span == elem_span` ⇒ excise the leaf in place.
        //  - `elem_span` = the leaf as written — `path.span` (which rustc makes
        //    *brace-relative*: `b::c` in `use a::{b::c, d}`, not `a::b::c`)
        //    extended through the binding ident, so it covers `B as C`. The
        //    intra-brace excision surface.
        //
        // rustc lowers each brace-list leaf to its own `Single` item, so
        // `visit_use` fires once per leaf and both spans are per-leaf-correct.
        // A glob carries `decl_span` too — its whole-statement delete surface
        // (the `glob` flag is its discriminator; it has no excisable leaf, so
        // `elem_span` stays `None`). List-stems carry neither.
        let (glob, alias, decl_span, binding_span) = match self.tcx.hir_node(hir_id) {
            Node::Item(Item {
                kind: ItemKind::Use(_, UseKind::Glob),
                span: decl,
                ..
            }) => (true, None, Some(*decl), None),
            Node::Item(Item {
                kind: ItemKind::Use(_, UseKind::Single(ident)),
                span: decl,
                ..
            }) => (
                false,
                Some(ident.to_string()),
                Some(*decl),
                Some(ident.span),
            ),
            _ => (false, None, None, None),
        };
        let elem_span = binding_span.map(|b| path.span.to(b));
        // The resolver's glob_map: which names actually resolved *through*
        // this glob — the same fact rustc's own `unused_imports` judgment
        // consults. Sorted: fragment bytes must be deterministic.
        let glob_used_names = if glob {
            let mut names: Vec<String> = self
                .tcx
                .resolutions(())
                .glob_map
                .get(&hir_id.owner.def_id)
                .into_iter()
                .flatten()
                .map(|s| s.to_string())
                .collect();
            names.sort();
            names
        } else {
            Vec::new()
        };
        for res in path.res.present_items() {
            if let Some(to) = res.opt_def_id() {
                self.record_import(
                    hir_id,
                    to,
                    path.span,
                    EdgeFlags {
                        import: true,
                        reexport,
                        glob,
                        alias: alias.clone(),
                        glob_used_names: glob_used_names.clone(),
                        via: self.via_crate(path.segments, to),
                        decl_span,
                        elem_span,
                        ..EdgeFlags::default()
                    },
                );
            }
        }
    }

    fn visit_item(&mut self, item: &'tcx Item<'tcx>) {
        // A module-level macro invocation leaves no HIR node of its own —
        // only generated items whose spans carry the expansion context.
        self.record_macro_expansion(item.span, item.hir_id());
        intravisit::walk_item(self, item);
    }

    fn visit_path(&mut self, path: &Path<'tcx>, id: HirId) {
        if let Some(to) = path.res.opt_def_id() {
            // Fully-qualified code paths (`shim::Item::call()`) carry the
            // written crate root exactly like `use` paths do.
            let via = self.via_crate(path.segments, to);
            let from_id = self.tcx.hir_get_parent_item(id).to_def_id();
            self.record_edge(
                from_id,
                to,
                Some(path.span),
                EdgeFlags {
                    via,
                    extern_root: self.extern_root(path.segments),
                    ..EdgeFlags::default()
                },
            );
        }
        intravisit::walk_path(self, path);
    }

    fn visit_expr(&mut self, ex: &'tcx Expr<'tcx>) {
        self.record_macro_expansion(ex.span, ex.hir_id);
        // Type-dependent resolutions path `Res` can't see — both read the
        // enclosing body's typeck results:
        //   * method calls `x.f()`                    (MethodCall)
        //   * type-relative value paths `Type::g`/`Type::CONST`
        //     (Path(QPath::TypeRelative), via `qpath_res`)
        match ex.kind {
            ExprKind::MethodCall(..) => {
                let owner = self.tcx.hir_enclosing_body_owner(ex.hir_id);
                if let Some(to) = self.tcx.typeck(owner).type_dependent_def_id(ex.hir_id) {
                    self.record_receiver(ex.hir_id, to, ex.span);
                }
            }
            ExprKind::Path(ref qpath @ QPath::TypeRelative(ty, _)) => {
                let owner = self.tcx.hir_enclosing_body_owner(ex.hir_id);
                if let Some(to) = self
                    .tcx
                    .typeck(owner)
                    .qpath_res(qpath, ex.hir_id)
                    .opt_def_id()
                {
                    // The value path's written ROOT is the type's (`$crate::
                    // Event::dispatch` — the whole path bypasses local
                    // imports iff the type part does), so extern_root must be
                    // inherited from it: the type's own edge carries it, but
                    // this edge's identity segments (`…::Event::dispatch`)
                    // are what feed the glob accounting's name evidence.
                    let extern_root = match ty.kind {
                        rustc_hir::TyKind::Path(QPath::Resolved(_, type_path)) => {
                            self.extern_root(type_path.segments)
                        }
                        _ => false,
                    };
                    let from_id = self.tcx.hir_get_parent_item(ex.hir_id).to_def_id();
                    self.record_edge(
                        from_id,
                        to,
                        Some(ex.span),
                        EdgeFlags {
                            extern_root,
                            ..EdgeFlags::default()
                        },
                    );
                }
            }
            // A plain assignment's LHS field is a WRITE — mark it so the
            // `Field` arm below skips it. rustc `dead_code` flags fields
            // that are only ever written (struct-literal inits emit no
            // `Field` expr at all, so they're never counted either).
            ExprKind::Assign(lhs, ..) => {
                if let ExprKind::Field(..) = lhs.kind {
                    self.write_fields.insert(lhs.hir_id);
                }
            }
            // A field READ (`x.f`, `t.0`) — the evidence the dead-field
            // narrow guard needs. Resolved via typeck (field access is not a
            // path). Tuples have no field defs; ADTs only.
            ExprKind::Field(base, _) => {
                if !self.write_fields.contains(&ex.hir_id) {
                    let owner = self.tcx.hir_enclosing_body_owner(ex.hir_id);
                    let typeck = self.tcx.typeck(owner);
                    if let Some(idx) = typeck.opt_field_index(ex.hir_id)
                        && let Some(adt) = typeck.expr_ty_adjusted(base).peel_refs().ty_adt_def()
                    {
                        let fd = &adt.non_enum_variant().fields[idx];
                        self.record_receiver(ex.hir_id, fd.did, ex.span);
                    }
                }
            }
            _ => {}
        }
        intravisit::walk_expr(self, ex);
    }

    /// Pattern field bindings are reads too (`let S { f, .. } = x`, match
    /// arms, fn params) — rustc `dead_code` counts them, so the dead-field
    /// guard must as well, or every pattern-destructured struct would gate
    /// its own narrow.
    fn visit_pat(&mut self, pat: &'tcx Pat<'tcx>) {
        match pat.kind {
            PatKind::Struct(ref qpath, fields, _) => {
                let owner = self.tcx.hir_enclosing_body_owner(pat.hir_id);
                let typeck = self.tcx.typeck(owner);
                if let Some(adt) = typeck.pat_ty(pat).peel_refs().ty_adt_def() {
                    let variant = adt.variant_of_res(typeck.qpath_res(qpath, pat.hir_id));
                    for f in fields {
                        if matches!(f.pat.kind, PatKind::Wild) {
                            continue; // `f: _` binds nothing — not a read
                        }
                        if let Some(fd) = variant
                            .fields
                            .iter()
                            .find(|fd| fd.ident(self.tcx).name == f.ident.name)
                        {
                            self.record_receiver(f.hir_id, fd.did, f.span);
                        }
                    }
                }
            }
            PatKind::TupleStruct(ref qpath, pats, dot_dot) => {
                let owner = self.tcx.hir_enclosing_body_owner(pat.hir_id);
                let typeck = self.tcx.typeck(owner);
                if let Some(adt) = typeck.pat_ty(pat).peel_refs().ty_adt_def() {
                    let variant = adt.variant_of_res(typeck.qpath_res(qpath, pat.hir_id));
                    for (i, p) in pats.iter().enumerate() {
                        if matches!(p.kind, PatKind::Wild) {
                            continue;
                        }
                        // Positions after a `..` gap map from the END.
                        let idx = match dot_dot.as_opt_usize() {
                            Some(gap) if i >= gap => variant.fields.len() - (pats.len() - i),
                            _ => i,
                        };
                        if let Some(fd) = variant.fields.iter().nth(idx) {
                            self.record_receiver(p.hir_id, fd.did, p.span);
                        }
                    }
                }
            }
            _ => {}
        }
        intravisit::walk_pat(self, pat);
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

/// If `def_id` is an **inherent-impl** associated item whose impl's self type
/// is a nominal type (ADT), the stable key of that type — the external-
/// reachability handle (see [`ItemFact::self_type`]): `Type::method` is
/// nameable from outside exactly when `Type` is, wherever the `impl` block
/// lives. `None` for trait-impl items (dispatch-judged via `trait_item`),
/// trait declarations, non-assoc defs, and non-ADT self types.
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

/// How an assoc fn takes `self`, from the HIR signature's implicit-self
/// classification (`fn(…)` ⇒ `none`, `self`/`mut self` ⇒ `value`, `&self` ⇒
/// `ref`, `&mut self` ⇒ `ref_mut`) — see [`ItemFact::self_kind`]. An explicit
/// typed receiver (`self: Box<Self>`) lowers as `None` here and is reported
/// `none`, the guard's under-flag direction. `None` for non-assoc-fn defs.
fn assoc_self_kind(tcx: TyCtxt<'_>, local_id: LocalDefId) -> Option<String> {
    if !matches!(tcx.def_kind(local_id.to_def_id()), DefKind::AssocFn) {
        return None;
    }
    let sig = tcx.hir_node_by_def_id(local_id).fn_sig()?;
    use rustc_hir::ImplicitSelfKind as Isk;
    let s = match sig.decl.implicit_self {
        Isk::None => "none",
        Isk::Imm | Isk::Mut => "value",
        Isk::RefImm => "ref",
        Isk::RefMut => "ref_mut",
    };
    Some(s.to_string())
}

/// For an assoc fn in an impl block: whether the impl's self type is `Copy`
/// (see [`ItemFact::self_copy`] — clippy's self-convention table is
/// `Copy`-sensitive). `None` for trait-declaration items (generic `Self`)
/// and non-assoc defs.
fn assoc_self_copy(tcx: TyCtxt<'_>, def_id: DefId) -> Option<bool> {
    if !matches!(tcx.def_kind(def_id), DefKind::AssocFn) {
        return None;
    }
    let parent = tcx.opt_parent(def_id)?;
    if !matches!(tcx.def_kind(parent), DefKind::Impl { .. }) {
        return None;
    }
    let self_ty = tcx.type_of(parent).skip_binder();
    let typing_env = ty::TypingEnv::non_body_analysis(tcx, parent);
    Some(tcx.type_is_copy_modulo_regions(typing_env, self_ty))
}

/// Climb `Ctor → (Variant →) ADT`: constructors and variants aren't tree
/// items; the def a use-site "uses" is the owning ADT.
fn projected_target(tcx: TyCtxt<'_>, mut did: DefId) -> DefId {
    while matches!(tcx.def_kind(did), DefKind::Ctor(..) | DefKind::Variant) {
        match tcx.opt_parent(did) {
            Some(parent) => did = parent,
            None => break,
        }
    }
    did
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
    // Proc-macro entry points are export roots too: their visibility is
    // forced (`functions tagged with #[proc_macro_derive] must be pub`) and
    // their only in-crate referrer is the compiler-synthesized `_DECLS`
    // registration — without this root the unused-pub verdict reads that
    // phantom edge as "only used inside the crate" and narrows illegally.
    if find_attr!(attrs, AttributeKind::ProcMacro(_))
        || find_attr!(attrs, AttributeKind::ProcMacroDerive { .. })
        || find_attr!(attrs, AttributeKind::ProcMacroAttribute(_))
    {
        out.push("proc_macro".to_string());
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
    // A macro can splice tokens from another file; a `hi` outside `lo`'s file
    // has no meaningful byte range there (and would underflow the relative
    // mapping below).
    if !sf.contains(hi) {
        return None;
    }
    // 1-based line of `lo`, from the file's own line table (computed on the
    // callsite-projected span, matching `lo`/`hi`). Diagnostic anchors need
    // it, and the extractor is the only place that has the SourceMap.
    let line = sf
        .lookup_line(sf.relative_position(lo))
        .map_or(0, |l| l + 1) as u32;
    // ON-DISK byte offsets, not rustc's internal ones: rustc normalizes a
    // source while loading (CRLF → LF, BOM strip) and all its positions live
    // in the normalized coordinates, but every consumer of `lo`/`hi` slices
    // the raw on-disk file — the probe checker and, critically, `--fix`
    // byte-range edits. In a CRLF file each preceding `\r` shifts the raw
    // position by one; map back through the file's normalization records.
    // (Line numbers are identical in both coordinate systems. For an LF file
    // the records are empty and this is the plain relative position.)
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
/// Kept verbatim in sync with the driver copy.
fn vis_span_to_ir(
    tcx: TyCtxt<'_>,
    sm: &rustc_span::source_map::SourceMap,
    local_id: LocalDefId,
) -> Option<Span> {
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

/// The whole-item deletion surface ([`ItemFact::full_span`]): the item
/// *including its body* (`span_with_body`) extended over its leading doc
/// comments and attributes, so a `--fix` deletion removes the item cleanly —
/// no orphaned `{ … }` block, no dangling `///`. `def_span` (what
/// [`ItemFact::span`] carries) is only the signature and would leave the body.
/// `None` for a macro-generated item (the span projects to the invocation, not
/// an editable surface).
fn full_item_span(
    tcx: TyCtxt<'_>,
    sm: &rustc_span::source_map::SourceMap,
    local_id: LocalDefId,
) -> Option<Span> {
    let hir_id = tcx.local_def_id_to_hir_id(local_id);
    let body = tcx.hir_span_with_body(hir_id);
    // `Span::to` encloses both operands, so folding over the attribute spans
    // extends `body` leftward to the earliest doc comment / attribute.
    let full = tcx
        .hir_attrs(hir_id)
        .iter()
        .filter_map(safe_attr_span)
        .fold(body, |acc, s| acc.to(s));
    if full.from_expansion() {
        return None;
    }
    span_to_ir(sm, full)
}

/// An attribute's source span, or `None` when it can't be retrieved.
/// `Attribute::span()` *panics* on most built-in `Parsed` attrs
/// (`#[macro_use]`, …), so we take only the two safe cases: doc comments (the
/// E0585-orphan hazard the deletion must cover) and `Unparsed` tool/custom
/// attributes. A skipped attribute is rare on a deletion candidate and, at
/// worst, is left in place — never deleted in error.
fn safe_attr_span(a: &rustc_hir::Attribute) -> Option<RustcSpan> {
    if let Some(s) = a.is_doc_comment() {
        return Some(s);
    }
    match a {
        rustc_hir::Attribute::Unparsed(u) => Some(u.span),
        _ => None,
    }
}

fn write_fragment(fragment: &IrFragment, file_stem: &str) {
    let out_dir = std::env::var("WL_IR_OUT").unwrap_or_else(|_| "target/wl-ir".to_string());
    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        eprintln!("WL-IR: create_dir_all({out_dir}) failed: {e}");
        return;
    }
    // The stem is the caller's business: unit-kind disambiguation (`@bin`,
    // `+test`, `@build`) lives in `check_crate`, next to the unit
    // discrimination that decides it. The `.wlir` extension marks the rkyv
    // transport (schema 7+); the assembler mmaps and reads it zero-copy.
    let path = format!("{out_dir}/{file_stem}.wlir");
    // Write-then-rename so a fragment is only ever observed complete: two
    // workspace-lint processes may extract the same workspace concurrently
    // (their compiles serialize on cargo's lock, but a reader in one can
    // otherwise catch the other's half-written buffer). The temp name carries
    // the pid so concurrent writers can't collide on it either. (Atomic
    // replace is also load-bearing for the mmap reader: `access_archive` is
    // unsound over a torn buffer, and rename never exposes one.)
    let tmp = format!("{path}.{}.tmp", std::process::id());
    match wl_ir::write_bytes(fragment) {
        Ok(bytes) => {
            match std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, &path)) {
                Ok(()) => eprintln!("WL-IR: wrote {} ({} items)", path, fragment.items.len()),
                Err(e) => eprintln!("WL-IR: write({path}) failed: {e}"),
            }
        }
        Err(e) => eprintln!("WL-IR: serialize failed: {e}"),
    }
}
