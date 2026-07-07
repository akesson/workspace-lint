//! The engine library of workspace-lint's rustc-fidelity backend.
//!
//! Two-phase architecture (`SPIKE-rustc-fidelity-tree.md` §4): Phase 1 runs a
//! nightly-pinned Dylint dylib (the vendored `extractor/` package) inside each
//! crate's compilation and writes per-crate `wl_ir::IrFragment`s; Phase 2 —
//! plain stable code, no `rustc_private` — assembles those fragments into the
//! workspace-global semantic model the lints query.
//!
//! This crate is the rustc-backed tier's pair of phases:
//!
//! - [`orchestrate`] — Phase-1 orchestration: the `[engine] configs` cargo
//!   command parser (`command.rs` → [`ConfigSpec`]), vendored-source
//!   materialization, toolchain preflight, the per-config extraction loop
//!   (embedded `dylint::run`), and the completeness guard.
//! - [`semantic`] — the Phase-2 assembler: fragments → per-config cross-crate
//!   join (`DefPathHash`) → cfg-matrix union (`(crate, def_path)`) → the
//!   verdict-producing queries the semantic lints consume.
//!
//! The build-free [`fast`] tier lives in its own leaf crate (`wl-fast`),
//! re-exported here — alongside its [`timing`] instrument — so consumers keep
//! their `wl_engine::fast::…` paths and see one engine surface. Neither crate
//! depends on the app layer (diagnostics, config, rendering) — "Phase 2 is
//! plain data" stays a structural boundary.

pub mod coverage;
pub mod orchestrate;
pub mod semantic;

/// The build-free fast tier (see the `wl-fast` crate).
pub use wl_fast as fast;
/// The `WL_TIMING` phase instrument, shared by both tiers and the binary.
pub use wl_fast::timing;

pub use orchestrate::{
    CommandError, ConfigRun, ConfigSpec, Engine, EngineConfig, EngineError, ExtractionRuns,
    ExtractorSource, Kinds, parse_command,
};
pub use semantic::{SemanticError, SemanticModel};
pub use wl_fast::FastModel;
// The IR contract is part of the semantic API surface (spans, fragments), so
// consumers get it from the engine — no separate wl-ir dependency needed.
pub use wl_ir;
