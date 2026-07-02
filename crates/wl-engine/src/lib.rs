//! The engine library of workspace-lint's rustc-fidelity backend.
//!
//! Two-phase architecture (`SPIKE-rustc-fidelity-tree.md` §4): Phase 1 runs a
//! nightly-pinned Dylint dylib (the vendored `extractor/` package) inside each
//! crate's compilation and writes per-crate `wl_ir::IrFragment`s; Phase 2 —
//! plain stable code, no `rustc_private` — assembles those fragments into the
//! workspace-global semantic model the lints query.
//!
//! This crate is the stable data layer for both of the binary's tiers:
//!
//! - [`orchestrate`] — Phase-1 orchestration: vendored-source materialization,
//!   toolchain preflight, the per-config extraction loop (embedded
//!   `dylint::run`), and the completeness guard.
//! - [`semantic`] — the Phase-2 assembler: fragments → per-config cross-crate
//!   join (`DefPathHash`) → cfg-matrix union (`(crate, def_path)`) → the
//!   verdict-producing queries the semantic lints consume.
//!
//! The build-free fast-tier model (`fast`) lands in the next migration PR;
//! keeping all tiers in one crate enforces the "Phase 2 is plain data"
//! boundary structurally — wl-engine never depends on the app layer
//! (diagnostics, config, rendering).

pub mod orchestrate;
pub mod semantic;

pub use orchestrate::{
    CfgSelector, ConfigRun, Engine, EngineConfig, EngineError, ExtractionRuns, ExtractorSource,
};
pub use semantic::{SemanticError, SemanticModel};
